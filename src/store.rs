//! On-disk stores (CPP_PKG.md; §8 of the implementation doc):
//! 1. raw download store   — packages as downloaded (git checkout / archive)
//! 2. package artifact store — installed prefix + manifest, content-addressed
//!    by (package id, config hash)
//! 3. sysdep store (wave 1, spec §5.3) — manifest-only entries for resolved
//!    system dependencies; never contains artifacts, machine-local by nature
//!
//! Layout (root default: ~/.cache/cpp-pkg, overridable via CPPKG_STORE env):
//!   <root>/raw/<depkey>-<pkgid-short>/          checkout or extracted archive
//!   <root>/raw/<depkey>-<base8>+<patch8>/       patched checkout (§5.2)
//!   <root>/pkg/<depkey>-<confighash>/install/   CMAKE_INSTALL_PREFIX
//!   <root>/pkg/<depkey>-<confighash>/manifest-e<EXTRACTOR_VERSION>.json
//!   <root>/sysdeps/<depkey>-<hash8>/            manifest.json + facts.json
//!   each entry carries a `.cppkg-entry.toml` marker: { schema-version,
//!   complete = true } written LAST — entries without complete=true are
//!   treated as absent (crash-safe).
//! Store paths are FINAL once created (no relocation). Coarse concurrency:
//! one exclusive flock on <root>/.lock held for the whole build (v0).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::Result;

/// File name of the per-entry completion marker.
const ENTRY_MARKER: &str = ".cppkg-entry.toml";

/// Length the raw-store package id is shortened to in directory names —
/// enough of a sha/blake3 prefix to be collision-free in practice while
/// keeping paths readable.
const PKGID_SHORT_LEN: usize = 16;

/// Length each half of a composed patched id is shortened to in raw-store
/// directory names ("absl-255c84da+a1b2c3d4"): 8 base chars + 8 patch-hash
/// chars keep the name readable while staying collision-free in practice —
/// and visibly distinct from unpatched entries.
const COMPOSED_SHORT_LEN: usize = 8;

/// Length the sysdep hash is shortened to in sysdep-store directory names.
const SYSDEP_SHORT_LEN: usize = 8;

/// Separator between base package id and patch-set hash in a composed id
/// (hashing::compose_patched_id). Parsed here so directory naming and id
/// composition can never drift apart silently.
const PATCH_SEP: &str = "+patches:";

/// Contents of `.cppkg-entry.toml`. Kebab-case on disk, matching every other
/// CppPkg TOML surface.
#[derive(Debug, Serialize, Deserialize)]
struct EntryMarker {
    #[serde(rename = "schema-version")]
    schema_version: u32,
    complete: bool,
}

pub struct Stores {
    pub root: PathBuf,
}

impl Stores {
    /// Default root (or $CPPKG_STORE); creates directories as needed.
    pub fn open_default() -> Result<Stores> {
        let root = match std::env::var_os("CPPKG_STORE") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => dirs::home_dir()
                .context("cannot determine home directory for the default store root; set CPPKG_STORE")?
                .join(".cache")
                .join("cpp-pkg"),
        };
        Stores::open(&root)
    }

    pub fn open(root: &Path) -> Result<Stores> {
        for sub in [
            root.to_path_buf(),
            root.join("raw"),
            root.join("pkg"),
            root.join("sysdeps"),
        ] {
            fs::create_dir_all(&sub)
                .with_context(|| format!("creating store directory {}", sub.display()))?;
        }
        Ok(Stores {
            root: root.to_path_buf(),
        })
    }

    /// Exclusive whole-build lock; guard releases on drop.
    pub fn lock(&self) -> Result<StoreLock> {
        let lock_path = self.root.join(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            // Never truncate: the file is a pure flock anchor, and truncating
            // an anchor another process holds open would be needless churn.
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening store lock file {}", lock_path.display()))?;
        // Blocks until any other build releases the store — coarse by design.
        file.lock_exclusive()
            .with_context(|| format!("acquiring exclusive lock on {}", lock_path.display()))?;
        Ok(StoreLock { _file: file })
    }

    /// Raw store entry path for (dep key, package id). `package_id` is a
    /// commit sha or "blake3:<hex>" (shortened to 16 chars in the dir name),
    /// or — since wave 1 (§5.2) — a composed patched id
    /// "<base>+patches:<hex>", rendered distinguishably as
    /// "<depkey>-<base8>+<patch8>" so a patched checkout can never be
    /// mistaken for (or reuse the entry of) the pristine one.
    pub fn raw_dir(&self, dep_key: &str, package_id: &str) -> PathBuf {
        let short = match package_id.split_once(PATCH_SEP) {
            Some((base, patch_hash)) => {
                let base = base.strip_prefix("blake3:").unwrap_or(base);
                let base8: String = base.chars().take(COMPOSED_SHORT_LEN).collect();
                let patch8: String = patch_hash.chars().take(COMPOSED_SHORT_LEN).collect();
                format!("{base8}+{patch8}")
            }
            None => {
                let id = package_id.strip_prefix("blake3:").unwrap_or(package_id);
                id.chars().take(PKGID_SHORT_LEN).collect()
            }
        };
        self.root.join("raw").join(format!("{dep_key}-{short}"))
    }

    /// Artifact store entry for (dep key, config hash).
    pub fn artifact_dir(&self, dep_key: &str, config_hash: &str) -> PathBuf {
        self.root.join("pkg").join(format!("{dep_key}-{config_hash}"))
    }

    /// Sysdep store entry for (dep key, hashing::sysdep_hash) — wave-1 §5.3.
    /// Manifest-only: holds manifest.json + facts.json (the resolved
    /// version/paths/file-hashes used to re-validate against the machine),
    /// never build artifacts. Uses the same completion-marker protocol as
    /// the other stores (entry_complete / mark_complete).
    pub fn sysdep_dir(&self, dep_key: &str, sysdep_hash: &str) -> PathBuf {
        let short: String = sysdep_hash.chars().take(SYSDEP_SHORT_LEN).collect();
        self.root.join("sysdeps").join(format!("{dep_key}-{short}"))
    }

    /// Extraction-manifest cache path inside an artifact entry (tool-fix
    /// A.8): the file name carries the extractor version, so bumping
    /// manifest::EXTRACTOR_VERSION re-derives manifests (cheap re-probe /
    /// re-read) without touching artifacts or their config-hash keys. Files
    /// written under older extractor versions are simply never read again —
    /// warm stores converge with fresh machines instead of keeping stale
    /// probe output forever.
    pub fn manifest_path(&self, entry_dir: &Path) -> PathBuf {
        entry_dir.join(format!(
            "manifest-e{}.json",
            crate::manifest::EXTRACTOR_VERSION
        ))
    }

    /// True iff the entry exists AND its marker says complete.
    pub fn entry_complete(&self, entry_dir: &Path) -> bool {
        let marker_path = entry_dir.join(ENTRY_MARKER);
        let Ok(text) = fs::read_to_string(&marker_path) else {
            return false;
        };
        // A malformed or truncated marker means the final write did not land
        // intact — treat exactly like a missing one.
        let Ok(marker) = toml::from_str::<EntryMarker>(&text) else {
            return false;
        };
        // A schema-version mismatch means the entry was written under a
        // different (incompatible) layout/hash encoding: unusable.
        marker.schema_version == crate::SCHEMA_VERSION && marker.complete
    }

    /// Write the completion marker (schema-version + complete=true). Call
    /// only after all entry contents are fully written.
    pub fn mark_complete(&self, entry_dir: &Path) -> Result<()> {
        let marker = EntryMarker {
            schema_version: crate::SCHEMA_VERSION,
            complete: true,
        };
        let text = toml::to_string(&marker).context("serializing store entry marker")?;

        // Write-to-temp + rename so a crash mid-write can never leave a
        // half-written marker that parses as complete. The pid suffix keeps
        // concurrent processes (should the coarse lock ever be bypassed) from
        // clobbering each other's temp file.
        let tmp_path = entry_dir.join(format!("{ENTRY_MARKER}.tmp.{}", std::process::id()));
        let marker_path = entry_dir.join(ENTRY_MARKER);
        (|| -> Result<()> {
            let mut file = fs::File::create(&tmp_path)?;
            use std::io::Write as _;
            file.write_all(text.as_bytes())?;
            // Flush contents to disk before the rename makes them visible.
            file.sync_all()?;
            fs::rename(&tmp_path, &marker_path)?;
            Ok(())
        })()
        .with_context(|| {
            format!("writing completion marker in {}", entry_dir.display())
        })
    }
}

pub struct StoreLock {
    // flock guard — implementation detail (fs2). Held for the whole build.
    // Dropping the File closes it, which releases the flock.
    _file: std::fs::File,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn storehash_open_creates_layout() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("store");
        let stores = Stores::open(&root).unwrap();
        assert_eq!(stores.root, root);
        assert!(root.join("raw").is_dir());
        assert!(root.join("pkg").is_dir());
        assert!(root.join("sysdeps").is_dir());
        // Re-opening an existing store is fine.
        Stores::open(&root).unwrap();
    }

    #[test]
    fn storehash_open_default_honors_env_override() {
        // Only this test reads CPPKG_STORE, so mutating the process env is
        // safe even under the parallel test runner.
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("env-store");
        // SAFETY: no other test in this crate reads or writes CPPKG_STORE,
        // and no test spawns threads that touch the environment.
        unsafe { std::env::set_var("CPPKG_STORE", &root) };
        let stores = Stores::open_default().unwrap();
        unsafe { std::env::remove_var("CPPKG_STORE") };
        assert_eq!(stores.root, root);
        assert!(root.join("raw").is_dir());
    }

    #[test]
    fn storehash_raw_dir_shortens_and_strips_label() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();

        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            stores.raw_dir("fmt", sha),
            stores.root.join("raw").join("fmt-0123456789abcdef")
        );

        let labeled = format!("blake3:{}", "f".repeat(64));
        assert_eq!(
            stores.raw_dir("zlib", &labeled),
            stores.root.join("raw").join(format!("zlib-{}", "f".repeat(16)))
        );

        // Ids shorter than the shortening length are used as-is.
        assert_eq!(
            stores.raw_dir("x", "abc"),
            stores.root.join("raw").join("x-abc")
        );
    }

    #[test]
    fn storehash_raw_dir_renders_composed_patched_ids() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();

        // Spec §5.2's worked example shape: absl-255c84da+a1b2c3d4.
        let composed = format!(
            "255c84dadd029fd8ad25c5efb5933e47beaa00c7+patches:a1b2c3d4{}",
            "e".repeat(24)
        );
        assert_eq!(
            stores.raw_dir("absl", &composed),
            stores.root.join("raw").join("absl-255c84da+a1b2c3d4")
        );

        // url deps: blake3 label on the base id is stripped before
        // shortening, exactly as for unpatched ids.
        let composed = format!("blake3:{}+patches:{}", "f".repeat(64), "0".repeat(32));
        assert_eq!(
            stores.raw_dir("zlib", &composed),
            stores
                .root
                .join("raw")
                .join(format!("zlib-{}+{}", "f".repeat(8), "0".repeat(8)))
        );

        // The patched entry must be distinct from the pristine one for the
        // same base commit — that is the whole point of the composed key.
        let sha = "255c84dadd029fd8ad25c5efb5933e47beaa00c7";
        let composed = crate::hashing::compose_patched_id(sha, &[b"delta".to_vec()]);
        assert_ne!(stores.raw_dir("absl", &composed), stores.raw_dir("absl", sha));
    }

    #[test]
    fn storehash_sysdep_dir_layout() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        assert_eq!(
            stores.sysdep_dir("zstd", "00112233445566778899aabbccddeeff"),
            stores.root.join("sysdeps").join("zstd-00112233")
        );
        // Marker protocol works unchanged on sysdep entries.
        let entry = stores.sysdep_dir("zstd", "00112233445566778899aabbccddeeff");
        fs::create_dir_all(&entry).unwrap();
        assert!(!stores.entry_complete(&entry));
        stores.mark_complete(&entry).unwrap();
        assert!(stores.entry_complete(&entry));
    }

    #[test]
    fn storehash_manifest_path_embeds_extractor_version() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let entry = stores.artifact_dir("fmt", "00112233445566778899aabbccddeeff");
        let path = stores.manifest_path(&entry);
        assert_eq!(path.parent().unwrap(), entry);
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("manifest-e{}.json", crate::manifest::EXTRACTOR_VERSION)
        );
        // A.8 requires the wave-1 extractor fixes to reach warm stores: the
        // version must have moved past the implicit v1 layout.
        const { assert!(crate::manifest::EXTRACTOR_VERSION >= 2) };
    }

    #[test]
    fn storehash_artifact_dir_layout() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        assert_eq!(
            stores.artifact_dir("fmt", "00112233445566778899aabbccddeeff"),
            stores
                .root
                .join("pkg")
                .join("fmt-00112233445566778899aabbccddeeff")
        );
    }

    #[test]
    fn storehash_entry_without_marker_is_incomplete() {
        // Crash-safety semantics: contents on disk but no marker (the crash
        // happened before mark_complete) must read as absent.
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let entry = stores.artifact_dir("fmt", "deadbeefdeadbeefdeadbeefdeadbeef");
        fs::create_dir_all(entry.join("install")).unwrap();
        fs::write(entry.join("manifest.json"), b"{}").unwrap();
        assert!(!stores.entry_complete(&entry));

        // Nonexistent entry likewise.
        let missing = stores.artifact_dir("fmt", "0000000000000000");
        assert!(!stores.entry_complete(&missing));
    }

    #[test]
    fn storehash_mark_complete_roundtrip() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let entry = stores.artifact_dir("fmt", "deadbeefdeadbeefdeadbeefdeadbeef");
        fs::create_dir_all(&entry).unwrap();
        stores.mark_complete(&entry).unwrap();
        assert!(stores.entry_complete(&entry));

        // Marker file is well-formed kebab-case TOML.
        let text = fs::read_to_string(entry.join(ENTRY_MARKER)).unwrap();
        assert!(text.contains("schema-version"));
        assert!(text.contains("complete = true"));

        // No temp file left behind.
        let leftovers: Vec<_> = fs::read_dir(&entry)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn storehash_partial_or_mismatched_marker_is_incomplete() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let entry = stores.artifact_dir("fmt", "deadbeefdeadbeefdeadbeefdeadbeef");
        fs::create_dir_all(&entry).unwrap();
        let marker = entry.join(ENTRY_MARKER);

        // Truncated / garbage marker.
        fs::write(&marker, "schema-vers").unwrap();
        assert!(!stores.entry_complete(&entry));

        // complete = false.
        fs::write(&marker, "schema-version = 1\ncomplete = false\n").unwrap();
        assert!(!stores.entry_complete(&entry));

        // Wrong schema version.
        fs::write(&marker, "schema-version = 999\ncomplete = true\n").unwrap();
        assert!(!stores.entry_complete(&entry));

        // Missing complete field.
        fs::write(&marker, "schema-version = 1\n").unwrap();
        assert!(!stores.entry_complete(&entry));

        // Correct marker fixes it.
        stores.mark_complete(&entry).unwrap();
        assert!(stores.entry_complete(&entry));
    }

    #[test]
    fn storehash_lock_acquire_release_reacquire() {
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let guard = stores.lock().unwrap();
        assert!(stores.root.join(".lock").exists());
        drop(guard);
        // Dropping the guard released the flock; a second acquisition must
        // not block.
        let _guard2 = stores.lock().unwrap();
    }

    #[test]
    fn storehash_lock_excludes_other_holders() {
        // flock is per open file description, so two opens within one
        // process do contend — enough to prove exclusion without a
        // subprocess.
        let tmp = tempdir().unwrap();
        let stores = Stores::open(tmp.path()).unwrap();
        let guard = stores.lock().unwrap();

        let lock_path = stores.root.join(".lock");
        let other = fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(
            other.try_lock_exclusive().is_err(),
            "second flock should be refused while the guard is held"
        );
        drop(guard);
        // Retry briefly: concurrently running tests spawn subprocesses, and
        // between fork and exec a child holds inherited duplicates of every
        // parent fd — including this lock's — so the release can lag a beat.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if other.try_lock_exclusive().is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "lock not reacquirable after guard drop"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
