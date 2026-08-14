//! Target-graph resolution and build planning.
//!
//! Responsibilities (CPPKG_TOML.md "Semantics"):
//! 1. NAMING LADDER for every dependency reference in [targets.*]:
//!    unique across manifests -> direct; else `<depkey>::` prefix owns; else
//!    exposes-namespace / exposes-targets (mapping form renames); else HARD
//!    ERROR listing candidate owning packages + the exposes-* fix.
//!    Local target names (no "::") resolve to sibling targets.
//! 2. VISIBILITY PROPAGATION: public deps/includes/defines propagate to
//!    consumers; private do not — EXCEPT private deps of a static-library
//!    propagate as LINK-ONLY edges (artifacts reach the final link closure,
//!    compile requirements stop). Manifest `requires` are public edges of
//!    that component; `link_requires` are link-only.
//! 3. SOURCES: expand globs relative to the project root in sorted byte
//!    order. Extension table (exhaustive, hard error otherwise):
//!    .cpp .cc .cxx .c++ -> C++ | .c -> C | .C -> error (case-insensitive
//!    FS) | .m .mm -> error ("Objective-C not supported in v0").
//! 4. INTERFACE_SOURCES of consumed components become CompileUnits of the
//!    consuming target (compiled with that component's usage requirements).
//! 5. LINK PLAN: topological order over the closure; static archives
//!    deduped keeping the LAST occurrence; frameworks/system libs deduped
//!    keeping first. Cycles among manifest components -> error (v0; group
//!    support later).
//! 6. LINK LANGUAGE: any C++ unit in the target or C++ anywhere in its
//!    closure -> C++ driver links.
//! 7. cxx-std: per-target `cxx-std` max-merged with the max `cxx_std`
//!    required by consumed components.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::manifest::Manifest;
use crate::schema::{BuildConfig, ProjectFile, TargetKind};
use crate::toolchain::Lang;
use crate::Result;

#[derive(Debug, Clone)]
pub struct CompileUnit {
    pub source: PathBuf,
    pub lang: Lang,
    pub std: Option<u32>,
    /// (dir, is_system)
    pub includes: Vec<(PathBuf, bool)>,
    pub defines: Vec<(String, Option<String>)>,
    pub extra_flags: Vec<String>,
    /// Object path relative to the build dir (unique per target+source).
    pub object: PathBuf,
}

#[derive(Debug, Clone)]
pub enum LinkInput {
    /// Objects of this target itself.
    Object(PathBuf),
    /// Static archive by absolute path (dep artifact or sibling target out).
    Archive(PathBuf),
    Dylib(PathBuf),
    SystemLib(String),
    Framework(String),
}

#[derive(Debug, Clone)]
pub struct PlannedTarget {
    pub name: String,
    pub kind: TargetKind,
    pub units: Vec<CompileUnit>,
    /// Output path relative to the build dir.
    pub output: PathBuf,
    /// Fully ordered link inputs (rule 5) — ninja_gen emits them verbatim.
    pub link_inputs: Vec<LinkInput>,
    pub link_flags: Vec<String>,
    pub link_lang: Lang,
    /// Sibling targets this one depends on (ninja dep edges).
    pub target_deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// Topological order (dependencies first).
    pub targets: Vec<PlannedTarget>,
}

/// Resolve + plan. `manifests` is keyed by dependency key. `only` restricts
/// to the named targets (+ their transitive sibling deps); empty = all.
pub fn plan(
    project: &ProjectFile,
    project_root: &std::path::Path,
    manifests: &BTreeMap<String, Manifest>,
    config: BuildConfig,
    profile_flags: &crate::schema::Profile,
    only: &[String],
) -> Result<BuildPlan> {
    let _ = (project, project_root, manifests, config, profile_flags, only);
    todo!()
}
