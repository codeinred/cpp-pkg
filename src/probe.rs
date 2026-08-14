//! Tier-2 extraction: probe an INSTALLED config-file package for its imported
//! targets and their usage requirements (CPP_PKG_IMPLEMENTATION.md §9).
//!
//! Mechanism (established in design):
//! - Generate a throwaway CMake project (in a temp dir under the artifact
//!   entry) that: snapshots the IMPORTED_TARGETS directory property, calls
//!   find_package(<find_name> REQUIRED CONFIG) with CMAKE_PREFIX_PATH set to
//!   the needs closure + this package's install dir, diffs the property to
//!   get the new imported targets, and for each target emits one
//!   file(GENERATE) whose CONTENT is built from $<TARGET_PROPERTY:...>
//!   generator expressions (file(GENERATE) evaluates genexes; use its TARGET
//!   argument for target-dependent expressions). Configured with the same
//!   toolchain file + CMAKE_BUILD_TYPE as the dependency build so per-config
//!   genexes flatten identically.
//! - WIRE FORMAT (decided): record-oriented text, NOT JSON — CMake cannot
//!   safely JSON-escape arbitrary property values. One record per
//!   (target, property): fields separated by \x1F (unit separator), records
//!   by \x1E (record separator):  target \x1F property \x1F value
//!   CMake ;-lists arrive as the raw value; splitting on unescaped ';' is
//!   done HERE in Rust (handle \; escapes).
//! - Properties probed per target (v0 frozen list):
//!   TYPE, IMPORTED_LOCATION_<CONFIG> (with fallbacks: IMPORTED_LOCATION,
//!   IMPORTED_LOCATION_RELEASE ... per CMake's config fallback rules),
//!   IMPORTED_IMPLIB is N/A on macOS, INTERFACE_INCLUDE_DIRECTORIES,
//!   INTERFACE_SYSTEM_INCLUDE_DIRECTORIES, INTERFACE_COMPILE_DEFINITIONS,
//!   INTERFACE_COMPILE_OPTIONS, INTERFACE_COMPILE_FEATURES,
//!   INTERFACE_LINK_LIBRARIES, INTERFACE_LINK_OPTIONS, INTERFACE_SOURCES,
//!   IMPORTED_LINK_INTERFACE_LANGUAGES.
//! - $<LINK_ONLY:...> in INTERFACE_LINK_LIBRARIES must SURVIVE to the
//!   records (probe emits both a genex-evaluated value and the raw property
//!   value for LINK_LIBRARIES so manifest.rs can distinguish link-only
//!   entries: property INTERFACE_LINK_LIBRARIES_RAW carries the unevaluated
//!   string).
//! - Failure modes: find_package failing in the probe is a bug in our prefix
//!   path assembly or the package -> surface the CMake log path.

use std::path::Path;

use crate::schema::BuildConfig;
use crate::toolchain::Toolchain;
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRecord {
    pub target: String,
    pub property: String,
    /// Raw single value as written by CMake (';'-splitting done by caller
    /// via `split_cmake_list`).
    pub value: String,
}

/// Run the probe. `find_name` is DependencySpec.find_package or the dep key.
/// `prefix_path` = needs closure installs + this package's install dir.
pub fn probe_installed(
    find_name: &str,
    prefix_path: &[std::path::PathBuf],
    config: BuildConfig,
    toolchain: &Toolchain,
    work_dir: &Path,
) -> Result<Vec<ProbeRecord>> {
    let _ = (find_name, prefix_path, config, toolchain, work_dir);
    todo!()
}

/// Split a CMake ;-list, honoring `\;` escapes.
pub fn split_cmake_list(value: &str) -> Vec<String> {
    let _ = value;
    todo!()
}

/// Parse the probe output file (\x1E records, \x1F fields).
pub fn parse_records(raw: &str) -> Result<Vec<ProbeRecord>> {
    let _ = raw;
    todo!()
}
