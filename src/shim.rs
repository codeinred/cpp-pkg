//! Secondary mode: CMake dependency provider (CPP_PKG_IMPLEMENTATION.md §4).
//!
//! Two artifacts:
//! 1. `<pkg>Config.cmake` SHIM emitted from a manifest: recreates each
//!    component as an IMPORTED target (add_library(<name> STATIC|SHARED|
//!    INTERFACE IMPORTED GLOBAL)) with IMPORTED_LOCATION_<CONFIG>,
//!    INTERFACE_INCLUDE_DIRECTORIES, INTERFACE_COMPILE_DEFINITIONS/OPTIONS,
//!    INTERFACE_LINK_LIBRARIES (link_requires wrapped in $<LINK_ONLY:...>),
//!    INTERFACE_LINK_OPTIONS, INTERFACE_SOURCES, cxx_std ->
//!    INTERFACE_COMPILE_FEATURES cxx_std_NN. Also emits
//!    <pkg>ConfigVersion.cmake accepting any version (v0).
//!    ROUND-TRIP INVARIANT: probing an emitted shim must reproduce the
//!    manifest (extract -> emit -> extract fixpoint) — keep property spelling
//!    exactly what probe.rs reads.
//! 2. `cppkg_provider.cmake`: injected via CMAKE_PROJECT_TOP_LEVEL_INCLUDES,
//!    calls cmake_language(SET_DEPENDENCY_PROVIDER cppkg_provide
//!    SUPPORTED_METHODS FIND_PACKAGE). The provider shells out to
//!    `cpp-pkg provide --package <name> ...` which resolves/builds from the
//!    store and prints the shim directory; the provider then find_package's
//!    it (NO_DEFAULT_PATH) and marks the request satisfied. FetchContent
//!    interception: deferred (FIND_PACKAGE only in v0).

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::Result;

/// Emit `<find_name>Config.cmake` (+ ConfigVersion) into `dir`; returns the
/// directory to put on CMAKE_PREFIX_PATH.
pub fn write_config_shim(manifest: &Manifest, find_name: &str, dir: &Path) -> Result<PathBuf> {
    let _ = (manifest, find_name, dir);
    todo!()
}

/// Emit the provider script; `cpp_pkg_bin` is the absolute path baked into
/// the script for shelling out.
pub fn write_provider_script(dir: &Path, cpp_pkg_bin: &Path) -> Result<PathBuf> {
    let _ = (dir, cpp_pkg_bin);
    todo!()
}
