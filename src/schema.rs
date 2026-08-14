//! `CppPkg.toml` types + parsing + validation. Normative spec: CPPKG_TOML.md.
//!
//! Implementation notes (contract):
//! - kebab-case TOML keys (`schema-version`, `cxx-std`, `exposes-namespace`).
//! - `VisibilitySplit` deserializes from EITHER a bare array (=> all private;
//!   sugar applies uniformly to includes/defines/dependencies) OR a table
//!   `{ public = [...], private = [...] }`.
//! - `DependencySpec` source: exactly one of git(+tag|rev) or url(+sha256);
//!   anything else is a validation error.
//! - Validation (all hard errors, with actionable messages):
//!   * charset `[a-zA-Z0-9_-]+` for package name, dependency keys, target names
//!   * `needs` entries must be dependency keys; `needs` cycles are errors
//!   * profile names must be one of the four built-ins (v0)
//!   * ABI-affecting profile flags are ALLOWED (they propagate to deps,
//!     see toolchain::classify_flags); `-fsanitize=*` triggers a warning
//!     (returned in `Warnings`, printed by the CLI)
//!   * unknown TOML keys should be rejected (serde deny_unknown_fields)

use std::collections::BTreeMap;
use std::path::Path;

use crate::Result;

/// The four CMake-compatible build configurations (v0 profiles; custom
/// profiles with `base-config` are reserved, not implemented).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildConfig {
    Debug,
    Release,
    RelWithDebInfo,
    MinSizeRel,
}

impl BuildConfig {
    /// CMake spelling: "Debug", "Release", "RelWithDebInfo", "MinSizeRel".
    pub fn cmake_name(self) -> &'static str {
        todo!()
    }
    /// TOML/CLI spelling: "debug", "release", "relwithdebinfo", "minsizerel".
    pub fn key(self) -> &'static str {
        todo!()
    }
    pub fn from_key(key: &str) -> Result<Self> {
        todo!()
    }
}

#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub package: PackageMeta,
    pub toolchains: BTreeMap<String, ToolchainPreset>,
    pub profiles: BTreeMap<String, Profile>,
    pub dependencies: BTreeMap<String, DependencySpec>,
    pub targets: BTreeMap<String, TargetSpec>,
}

#[derive(Debug, Clone)]
pub struct PackageMeta {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolchainPreset {
    pub cxx: String,
    /// Derived from `cxx` at detection time if absent.
    pub cc: Option<String>,
    pub ar: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub cxx_flags: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum GitRef {
    Tag(String),
    Rev(String),
}

#[derive(Debug, Clone)]
pub enum SourceSpec {
    Git { url: String, reference: GitRef },
    Url { url: String, sha256: String },
}

/// `exposes-targets`: list form claims ownership; map form also renames
/// (extracted name -> exposed name).
#[derive(Debug, Clone, Default)]
pub struct ExposesTargets {
    pub claims: Vec<String>,
    pub renames: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DependencySpec {
    pub source: SourceSpec,
    /// CMake cache options. Hashed as LITERAL strings — never normalized.
    pub options: BTreeMap<String, String>,
    /// find_dependency edges; drives build order + CMAKE_PREFIX_PATH closure.
    pub needs: Vec<String>,
    /// `find_package(<this>)` name used by the probe; defaults to the dep key.
    /// (Schema addition over CPPKG_TOML.md — recorded in DESIGN_CHOICES.md.)
    pub find_package: Option<String>,
    pub exposes_namespace: Vec<String>,
    pub exposes_targets: ExposesTargets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Executable,
    StaticLibrary,
}

/// public/private lists; bare array deserializes as all-private.
#[derive(Debug, Clone, Default)]
pub struct VisibilitySplit {
    pub public: Vec<String>,
    pub private: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub kind: TargetKind,
    /// Glob patterns; expansion (sorted byte order) happens in graph::plan.
    pub sources: Vec<String>,
    pub cxx_std: Option<u32>,
    pub c_std: Option<u32>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
}

/// Non-fatal findings surfaced to the user by the CLI (e.g. sanitizer flags
/// present: dependencies are uninstrumented).
#[derive(Debug, Clone, Default)]
pub struct Warnings(pub Vec<String>);

/// Parse + validate a CppPkg.toml. Fails with `schema-version` mismatch,
/// syntax errors, or any validation rule above.
pub fn load(path: &Path) -> Result<(ProjectFile, Warnings)> {
    let _ = path;
    todo!()
}

/// Topological order of dependency keys following `needs` edges
/// (dependencies before dependents). Cycle => error naming the cycle.
pub fn dependency_build_order(deps: &BTreeMap<String, DependencySpec>) -> Result<Vec<String>> {
    let _ = deps;
    todo!()
}

/// Transitive closure of `needs` for one dependency (for CMAKE_PREFIX_PATH:
/// a loaded fmtConfig.cmake re-runs its own find_dependency calls).
pub fn needs_closure(
    deps: &BTreeMap<String, DependencySpec>,
    key: &str,
) -> Result<Vec<String>> {
    let _ = (deps, key);
    todo!()
}
