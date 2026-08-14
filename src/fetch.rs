//! Dependency acquisition into the raw download store.
//!
//! Contract (CPPKG_TOML.md lockfile section; DESIGN_CHOICES.md):
//! - git + tag: resolve tag -> commit (ls-remote or clone+rev-parse), clone
//!   at that commit into the raw store; the COMMIT SHA is the package id and
//!   integrity reference (verify via rev-parse after checkout). Tags are
//!   mutable; the lockfile pin (commit) wins over re-resolving the tag.
//! - git + rev: fetch that commit directly.
//! - url + sha256: download (ureq), verify user-declared sha256, package id
//!   is "blake3:<hex>" of the archive bytes; extract tar.gz/zip into the raw
//!   store entry (strip a single top-level directory if the archive has one).
//! - SUBMODULES: if the checkout contains a .gitmodules file -> hard error
//!   ("git submodules are not supported in v0"), do not build silently.
//! - Fresh-machine flow: lockfile commit + source URL suffice to re-download.

use std::path::PathBuf;

use crate::lockfile::LockedPackage;
use crate::schema::DependencySpec;
use crate::store::Stores;
use crate::Result;

#[derive(Debug, Clone)]
pub struct RawPackage {
    /// Directory containing the package source tree (raw store entry).
    pub path: PathBuf,
    /// Commit sha (git) or "blake3:<hex>" (url) — feeds hashing + lockfile.
    pub package_id: String,
}

/// Ensure the dependency's source is present in the raw store; returns the
/// entry. Uses `locked` (if it matches the request) instead of re-resolving
/// mutable refs. Network is touched only when the store lacks the entry.
pub fn ensure(
    stores: &Stores,
    dep_key: &str,
    spec: &DependencySpec,
    locked: Option<&LockedPackage>,
) -> Result<RawPackage> {
    let _ = (stores, dep_key, spec, locked);
    todo!()
}
