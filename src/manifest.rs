//! Package manifests: CPS-style JSON stored beside each artifact entry
//! (CPP_PKG_IMPLEMENTATION.md §2 — CPS with vendor extensions; the on-disk
//! format is CPS-shaped but only the fields we consume are guaranteed).
//!
//! Contract:
//! - Component names are the full exported names ("fmt::fmt").
//! - `from_probe` consumes probe records for ONE config; `location` is keyed
//!   by CMake config name so future configs merge additively.
//! - INTERFACE_LINK_LIBRARIES entries resolve to: another component in this
//!   package -> `requires`; a $<LINK_ONLY:x> entry -> `link_requires`;
//!   an absolute path -> `link_paths`; `-lfoo`/plain name -> `system_libs`;
//!   `-framework X` / FRAMEWORK genex or Foo.framework path -> `frameworks`;
//!   a target from ANOTHER package (transitive find_dependency) ->
//!   `requires` with its full name (cross-package refs resolved at graph
//!   time via the naming ladder).
//! - Compile features: only cxx_std_NN is honored (mapped to a std level,
//!   max-merged with cxx-std at graph time); other features are ignored
//!   with a recorded warning (granular features are pre-C++17 legacy).
//! - INTERFACE_SOURCES: paths recorded; consumer compiles them (decided).
//! - Deduplicate targets claimed by multiple probes (transitive
//!   find_dependency): a component whose defining package is ambiguous keeps
//!   ONLY the attribution decided by graph::resolve via exposes-* /
//!   namespace matching — from_probe records everything it saw plus which
//!   find_package call surfaced it (`origin_find_name`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::probe::ProbeRecord;
use crate::schema::BuildConfig;
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentKind {
    /// STATIC_LIBRARY -> archive
    Archive,
    /// SHARED_LIBRARY -> dylib (consumable in v0 even though we BUILD static)
    Dylib,
    /// INTERFACE_LIBRARY (header-only)
    Interface,
    /// UNKNOWN imported type: treat location as link input if present
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct Component {
    pub kind: Option<ComponentKind>,
    /// CMake config name -> artifact path.
    pub location: BTreeMap<String, PathBuf>,
    pub includes: Vec<PathBuf>,
    pub system_includes: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub compile_options: Vec<String>,
    /// Max cxx_std_NN seen, if any.
    pub cxx_std: Option<u32>,
    pub link_options: Vec<String>,
    /// Full names of required components (compile + link propagation).
    pub requires: Vec<String>,
    /// Link-only requirements ($<LINK_ONLY:...>): artifacts reach the link
    /// closure, compile requirements do not propagate.
    pub link_requires: Vec<String>,
    pub link_paths: Vec<PathBuf>,
    pub system_libs: Vec<String>,
    pub frameworks: Vec<String>,
    pub interface_sources: Vec<PathBuf>,
    /// Which find_package() surfaced this component (attribution input).
    pub origin_find_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    /// Dependency key this manifest belongs to.
    pub package: String,
    pub components: BTreeMap<String, Component>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        let _ = path;
        todo!()
    }
    /// Stable field order / sorted maps for deterministic files.
    pub fn save(&self, path: &Path) -> Result<()> {
        let _ = path;
        todo!()
    }
}

/// Build a manifest from one probe run (one config).
pub fn from_probe(
    dep_key: &str,
    find_name: &str,
    config: BuildConfig,
    records: &[ProbeRecord],
) -> Result<Manifest> {
    let _ = (dep_key, find_name, config, records);
    todo!()
}
