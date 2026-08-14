//! `CppPkg.lock` — grammar is lockfile ABI, pinned in CPPKG_TOML.md:
//!   source    = "git+<url>" | "url+<url>"
//!   requested = "tag:<tag>" | "rev:<sha>" | "sha256:<hex>"
//!   commit present iff git (pin + integrity + re-download reference;
//!     decided: the commit sha IS the content hash for git deps in v0)
//!   content-hash present iff url ("blake3:<hex>" of archive bytes)
//! Written/updated on every resolve; `options`/`needs` deliberately absent.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// Keyed by dependency key ("name" field in the TOML array-of-tables).
    pub packages: BTreeMap<String, LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub source: String,
    pub requested: String,
    pub commit: Option<String>,
    pub content_hash: Option<String>,
}

/// On-disk shape. Kept separate from the public types so the file format
/// (array-of-tables with an inline `name`) can't drift from the in-memory
/// map representation without a deliberate change here.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LockDoc {
    schema_version: u32,
    #[serde(default, rename = "package")]
    packages: Vec<PackageDoc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct PackageDoc {
    name: String,
    source: String,
    requested: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
}

/// Reject entries that don't satisfy the pinned grammar. A lockfile is ABI:
/// silently accepting a malformed entry would let a corrupt pin masquerade
/// as a valid one, so every invariant is a hard error naming the package.
fn validate_entry(name: &str, pkg: &LockedPackage) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("lockfile: invalid package name {name:?} (allowed charset: [a-zA-Z0-9_-]+)");
    }
    let is_git = match pkg.source.split_once('+') {
        Some(("git", rest)) if !rest.is_empty() => true,
        Some(("url", rest)) if !rest.is_empty() => false,
        _ => bail!(
            "lockfile: package {name:?} has invalid source {:?} \
             (expected \"git+<url>\" or \"url+<url>\")",
            pkg.source
        ),
    };
    let req_kind = match pkg.requested.split_once(':') {
        Some((kind @ ("tag" | "rev" | "sha256"), rest)) if !rest.is_empty() => kind,
        _ => bail!(
            "lockfile: package {name:?} has invalid requested {:?} \
             (expected \"tag:<tag>\", \"rev:<sha>\", or \"sha256:<hex>\")",
            pkg.requested
        ),
    };
    match (is_git, req_kind) {
        (true, "tag" | "rev") | (false, "sha256") => {}
        (true, _) => bail!(
            "lockfile: package {name:?} is a git source but requested {:?} \
             (git sources use tag:<tag> or rev:<sha>)",
            pkg.requested
        ),
        (false, _) => bail!(
            "lockfile: package {name:?} is a url source but requested {:?} \
             (url sources use sha256:<hex>)",
            pkg.requested
        ),
    }
    // `commit` iff git, `content-hash` iff url: each field is the integrity
    // anchor for exactly one source kind, and its absence (or presence on the
    // wrong kind) means the entry cannot be trusted as a pin.
    match (is_git, &pkg.commit, &pkg.content_hash) {
        (true, Some(_), None) | (false, None, Some(_)) => Ok(()),
        (true, None, _) => bail!("lockfile: git package {name:?} is missing `commit`"),
        (true, Some(_), Some(_)) => {
            bail!("lockfile: git package {name:?} must not have `content-hash`")
        }
        (false, _, None) => bail!("lockfile: url package {name:?} is missing `content-hash`"),
        (false, Some(_), _) => bail!("lockfile: url package {name:?} must not have `commit`"),
    }
}

impl Lockfile {
    pub fn load(path: &Path) -> Result<Option<Lockfile>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("reading lockfile {}", path.display()))
            }
        };
        let doc: LockDoc = toml::from_str(&text)
            .with_context(|| format!("parsing lockfile {}", path.display()))?;
        if doc.schema_version != crate::SCHEMA_VERSION {
            bail!(
                "lockfile {}: schema-version {} not supported (this cpp-pkg supports {})",
                path.display(),
                doc.schema_version,
                crate::SCHEMA_VERSION
            );
        }
        let mut packages = BTreeMap::new();
        for p in doc.packages {
            let entry = LockedPackage {
                source: p.source,
                requested: p.requested,
                commit: p.commit,
                content_hash: p.content_hash,
            };
            validate_entry(&p.name, &entry)?;
            if packages.insert(p.name.clone(), entry).is_some() {
                bail!("lockfile {}: duplicate package {:?}", path.display(), p.name);
            }
        }
        Ok(Some(Lockfile { packages }))
    }

    /// Deterministic output (sorted by name) so lockfile diffs are stable.
    pub fn save(&self, path: &Path) -> Result<()> {
        // BTreeMap iteration is already name-sorted, which is the whole
        // determinism story: same packages => byte-identical file.
        let mut docs = Vec::with_capacity(self.packages.len());
        for (name, pkg) in &self.packages {
            validate_entry(name, pkg)?;
            docs.push(PackageDoc {
                name: name.clone(),
                source: pkg.source.clone(),
                requested: pkg.requested.clone(),
                commit: pkg.commit.clone(),
                content_hash: pkg.content_hash.clone(),
            });
        }
        let doc = LockDoc { schema_version: crate::SCHEMA_VERSION, packages: docs };
        let body = toml::to_string(&doc).context("serializing lockfile")?;
        // Header comment marks the file as generated; kept ahead of the TOML
        // so tooling that ignores comments sees a plain document.
        let text = format!("# Generated by cpp-pkg. Commit this file to version control.\n{body}");
        std::fs::write(path, text)
            .with_context(|| format!("writing lockfile {}", path.display()))
    }

    /// Returns the entry only if it still matches what CppPkg.toml requests
    /// (same source + requested); a changed request invalidates the pin.
    pub fn matching_entry(&self, key: &str, source: &str, requested: &str) -> Option<&LockedPackage> {
        self.packages
            .get(key)
            .filter(|p| p.source == source && p.requested == requested)
    }
}

/// Render `source` / `requested` strings from a schema::SourceSpec (the only
/// place this grammar is produced — keep it here).
pub fn source_string(spec: &crate::schema::SourceSpec) -> String {
    use crate::schema::SourceSpec;
    match spec {
        SourceSpec::Git { url, .. } => format!("git+{url}"),
        SourceSpec::Url { url, .. } => format!("url+{url}"),
    }
}

pub fn requested_string(spec: &crate::schema::SourceSpec) -> String {
    use crate::schema::{GitRef, SourceSpec};
    match spec {
        SourceSpec::Git { reference: GitRef::Tag(tag), .. } => format!("tag:{tag}"),
        SourceSpec::Git { reference: GitRef::Rev(rev), .. } => format!("rev:{rev}"),
        SourceSpec::Url { sha256, .. } => format!("sha256:{sha256}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GitRef, SourceSpec};

    fn git_pkg() -> LockedPackage {
        LockedPackage {
            source: "git+https://github.com/fmtlib/fmt".into(),
            requested: "tag:11.2.0".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            content_hash: None,
        }
    }

    fn url_pkg() -> LockedPackage {
        LockedPackage {
            source: "url+https://zlib.net/zlib-1.3.1.tar.gz".into(),
            requested: "sha256:9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23".into(),
            commit: None,
            content_hash: Some("blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262".into()),
        }
    }

    fn sample() -> Lockfile {
        let mut packages = BTreeMap::new();
        packages.insert("fmt".to_string(), git_pkg());
        packages.insert("zlib".to_string(), url_pkg());
        Lockfile { packages }
    }

    #[test]
    fn lockfile_load_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Lockfile::load(&dir.path().join("CppPkg.lock")).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn lockfile_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");
        let lock = sample();
        lock.save(&path).unwrap();
        let loaded = Lockfile::load(&path).unwrap().expect("file exists");
        assert_eq!(loaded, lock);
    }

    #[test]
    fn lockfile_save_is_byte_stable() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.lock");
        let b = dir.path().join("b.lock");
        let lock = sample();
        lock.save(&a).unwrap();
        // Round-trip through load to prove parsing doesn't perturb output,
        // then save again: bytes must be identical.
        let reloaded = Lockfile::load(&a).unwrap().unwrap();
        reloaded.save(&b).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    }

    #[test]
    fn lockfile_output_is_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");
        // BTreeMap sorts regardless of insertion order; assert the rendered
        // file reflects that ordering.
        let mut packages = BTreeMap::new();
        packages.insert("zlib".to_string(), url_pkg());
        packages.insert("fmt".to_string(), git_pkg());
        Lockfile { packages }.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let fmt_at = text.find("name = \"fmt\"").unwrap();
        let zlib_at = text.find("name = \"zlib\"").unwrap();
        assert!(fmt_at < zlib_at);
        assert!(text.contains("schema-version = 1"));
        assert!(text.contains("content-hash = "));
    }

    #[test]
    fn lockfile_schema_version_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");
        std::fs::write(&path, "schema-version = 99\n").unwrap();
        let err = Lockfile::load(&path).unwrap_err().to_string();
        assert!(err.contains("schema-version 99"), "got: {err}");
    }

    #[test]
    fn lockfile_matching_entry_invalidates_on_change() {
        let lock = sample();
        let source = "git+https://github.com/fmtlib/fmt";
        let requested = "tag:11.2.0";
        assert!(lock.matching_entry("fmt", source, requested).is_some());
        // Changed requested ref => pin no longer applies.
        assert!(lock.matching_entry("fmt", source, "tag:12.0.0").is_none());
        // Changed source URL => pin no longer applies.
        assert!(lock
            .matching_entry("fmt", "git+https://example.com/fmt", requested)
            .is_none());
        // Unknown key.
        assert!(lock.matching_entry("nope", source, requested).is_none());
    }

    #[test]
    fn lockfile_grammar_producers() {
        let git_tag = SourceSpec::Git {
            url: "https://github.com/fmtlib/fmt".into(),
            reference: GitRef::Tag("11.2.0".into()),
        };
        let git_rev = SourceSpec::Git {
            url: "https://github.com/fmtlib/fmt".into(),
            reference: GitRef::Rev("abc123".into()),
        };
        let url = SourceSpec::Url {
            url: "https://zlib.net/zlib-1.3.1.tar.gz".into(),
            sha256: "deadbeef".into(),
        };
        assert_eq!(source_string(&git_tag), "git+https://github.com/fmtlib/fmt");
        assert_eq!(source_string(&url), "url+https://zlib.net/zlib-1.3.1.tar.gz");
        assert_eq!(requested_string(&git_tag), "tag:11.2.0");
        assert_eq!(requested_string(&git_rev), "rev:abc123");
        assert_eq!(requested_string(&url), "sha256:deadbeef");
    }

    #[test]
    fn lockfile_invariants_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");

        // git entry missing commit
        let mut bad = git_pkg();
        bad.commit = None;
        let mut packages = BTreeMap::new();
        packages.insert("fmt".to_string(), bad);
        let err = Lockfile { packages }.save(&path).unwrap_err().to_string();
        assert!(err.contains("missing `commit`"), "got: {err}");

        // url entry carrying a commit
        let mut bad = url_pkg();
        bad.commit = Some("abc".into());
        let mut packages = BTreeMap::new();
        packages.insert("zlib".to_string(), bad);
        let err = Lockfile { packages }.save(&path).unwrap_err().to_string();
        assert!(err.contains("must not have `commit`"), "got: {err}");

        // git source with a sha256 request (kind mismatch)
        let mut bad = git_pkg();
        bad.requested = "sha256:abcd".into();
        let mut packages = BTreeMap::new();
        packages.insert("fmt".to_string(), bad);
        let err = Lockfile { packages }.save(&path).unwrap_err().to_string();
        assert!(err.contains("git sources use tag:"), "got: {err}");

        // unrecognized source scheme
        let mut bad = git_pkg();
        bad.source = "hg+https://example.com/repo".into();
        let mut packages = BTreeMap::new();
        packages.insert("fmt".to_string(), bad);
        let err = Lockfile { packages }.save(&path).unwrap_err().to_string();
        assert!(err.contains("invalid source"), "got: {err}");

        // malformed entries are also rejected on load, not just save
        std::fs::write(
            &path,
            "schema-version = 1\n\n[[package]]\nname = \"fmt\"\nsource = \"git+https://x\"\nrequested = \"tag:1\"\n",
        )
        .unwrap();
        let err = Lockfile::load(&path).unwrap_err().to_string();
        assert!(err.contains("missing `commit`"), "got: {err}");
    }

    #[test]
    fn lockfile_duplicate_name_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");
        let entry = "[[package]]\nname = \"fmt\"\nsource = \"git+https://x\"\nrequested = \"tag:1\"\ncommit = \"abc\"\n";
        std::fs::write(&path, format!("schema-version = 1\n\n{entry}\n{entry}")).unwrap();
        let err = Lockfile::load(&path).unwrap_err().to_string();
        assert!(err.contains("duplicate package"), "got: {err}");
    }

    #[test]
    fn lockfile_unknown_key_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CppPkg.lock");
        std::fs::write(
            &path,
            "schema-version = 1\n\n[[package]]\nname = \"fmt\"\nsource = \"git+https://x\"\nrequested = \"tag:1\"\ncommit = \"abc\"\nbogus = 1\n",
        )
        .unwrap();
        assert!(Lockfile::load(&path).is_err());
    }
}
