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

use std::path::{Path, PathBuf};

use crate::Result;

pub struct Stores {
    pub root: PathBuf,
}

impl Stores {
    /// Default root (or $CPPKG_STORE); creates directories as needed.
    pub fn open_default() -> Result<Stores> {
        todo!()
    }
    pub fn open(root: &Path) -> Result<Stores> {
        let _ = root;
        todo!()
    }
    /// Exclusive whole-build lock; guard releases on drop.
    pub fn lock(&self) -> Result<StoreLock> {
        todo!()
    }

    /// Raw store entry path for (dep key, package id). `package_id` is a
    /// commit sha or "blake3:<hex>" (shortened to 16 chars in the dir name).
    pub fn raw_dir(&self, dep_key: &str, package_id: &str) -> PathBuf {
        let _ = (dep_key, package_id);
        todo!()
    }
    /// Artifact store entry for (dep key, config hash).
    pub fn artifact_dir(&self, dep_key: &str, config_hash: &str) -> PathBuf {
        let _ = (dep_key, config_hash);
        todo!()
    }

    /// True iff the entry exists AND its marker says complete.
    pub fn entry_complete(&self, entry_dir: &Path) -> bool {
        let _ = entry_dir;
        todo!()
    }
    /// Write the completion marker (schema-version + complete=true). Call
    /// only after all entry contents are fully written.
    pub fn mark_complete(&self, entry_dir: &Path) -> Result<()> {
        let _ = entry_dir;
        todo!()
    }
}

pub struct StoreLock {
    // flock guard — implementation detail (fs2). Held for the whole build.
    _file: std::fs::File,
}
