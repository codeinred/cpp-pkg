//! `CppPkg.lock` — grammar is lockfile ABI, pinned in CPPKG_TOML.md:
//!   source    = "git+<url>" | "url+<url>"
//!   requested = "tag:<tag>" | "rev:<sha>" | "sha256:<hex>"
//!   commit present iff git (pin + integrity + re-download reference;
//!     decided: the commit sha IS the content hash for git deps in v0)
//!   content-hash present iff url ("blake3:<hex>" of archive bytes)
//! Written/updated on every resolve; `options`/`needs` deliberately absent.

use std::collections::BTreeMap;
use std::path::Path;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Lockfile {
    /// Keyed by dependency key ("name" field in the TOML array-of-tables).
    pub packages: BTreeMap<String, LockedPackage>,
}

#[derive(Debug, Clone)]
pub struct LockedPackage {
    pub source: String,
    pub requested: String,
    pub commit: Option<String>,
    pub content_hash: Option<String>,
}

impl Lockfile {
    pub fn load(path: &Path) -> Result<Option<Lockfile>> {
        let _ = path;
        todo!()
    }
    /// Deterministic output (sorted by name) so lockfile diffs are stable.
    pub fn save(&self, path: &Path) -> Result<()> {
        let _ = path;
        todo!()
    }
    /// Returns the entry only if it still matches what CppPkg.toml requests
    /// (same source + requested); a changed request invalidates the pin.
    pub fn matching_entry(&self, key: &str, source: &str, requested: &str) -> Option<&LockedPackage> {
        let _ = (key, source, requested);
        todo!()
    }
}

/// Render `source` / `requested` strings from a schema::SourceSpec (the only
/// place this grammar is produced — keep it here).
pub fn source_string(spec: &crate::schema::SourceSpec) -> String {
    let _ = spec;
    todo!()
}
pub fn requested_string(spec: &crate::schema::SourceSpec) -> String {
    let _ = spec;
    todo!()
}
