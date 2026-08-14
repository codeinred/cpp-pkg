//! On-disk stores (CPP_PKG.md; §8 of the implementation doc):
//! 1. raw download store   — packages as downloaded (git checkout / archive)
//! 2. package artifact store — installed prefix + manifest, content-addressed
//!    by (package id, config hash)
//!
//! Layout (root default: ~/.cache/cpp-pkg, overridable via CPPKG_STORE env):
//!   <root>/raw/<depkey>-<pkgid-short>/          checkout or extracted archive
//!   <root>/pkg/<depkey>-<confighash>/install/   CMAKE_INSTALL_PREFIX
//!   <root>/pkg/<depkey>-<confighash>/manifest.json
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
        for sub in [root.to_path_buf(), root.join("raw"), root.join("pkg")] {
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
    /// commit sha or "blake3:<hex>" (shortened to 16 chars in the dir name).
    pub fn raw_dir(&self, dep_key: &str, package_id: &str) -> PathBuf {
        let id = package_id.strip_prefix("blake3:").unwrap_or(package_id);
        let short: String = id.chars().take(PKGID_SHORT_LEN).collect();
        self.root.join("raw").join(format!("{dep_key}-{short}"))
    }

    /// Artifact store entry for (dep key, config hash).
    pub fn artifact_dir(&self, dep_key: &str, config_hash: &str) -> PathBuf {
        self.root.join("pkg").join(format!("{dep_key}-{config_hash}"))
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
