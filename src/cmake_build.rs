//! Dependency builds via CMake (CPP_PKG_IMPLEMENTATION.md §3, §5).
//!
//! Contract:
//! - Generate a toolchain file from the detected toolchain (single source of
//!   truth): sets CMAKE_C(XX)_COMPILER, CMAKE_OSX_SYSROOT (if SDK), CMAKE_AR,
//!   and appends ABI-classified profile flags to CMAKE_C_FLAGS_INIT /
//!   CMAKE_CXX_FLAGS_INIT. Deps never pick a compiler from the environment.
//! - Configure with: -G Ninja, CMAKE_BUILD_TYPE=<config>,
//!   CMAKE_TOOLCHAIN_FILE, CMAKE_INSTALL_PREFIX=<artifact entry>/install,
//!   BUILD_SHARED_LIBS=OFF (overridable by dep options), the dep's literal
//!   `options`, CMAKE_POLICY_VERSION_MINIMUM=3.5 (CMake 4.x dropped <3.5
//!   compat and many real deps still declare old minimums),
//!   CMAKE_PREFIX_PATH=<install dirs of the TRANSITIVE needs closure>.
//! - Scrub the environment: pass a minimal env (PATH, HOME, and the vars
//!   CMake/compilers genuinely need); drop CC/CXX/CFLAGS/CXXFLAGS/
//!   LDFLAGS/CMAKE_* so host config can't leak into hashed builds.
//! - Build + install (cmake --build . && cmake --install .); on success the
//!   caller writes the manifest and marks the store entry complete.
//! - ERROR TRANSLATION (§5): scan the configure log for
//!   `find_dependency`/`find_package` failures and translate BOTH shapes:
//!   not-found -> "add <pkg> to [dependencies] and to <dep>.needs";
//!   version-rejection -> name the pinned version vs the requirement.
//!   Preserve the raw CMake log path in the error for debugging.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::schema::{BuildConfig, DependencySpec};
use crate::toolchain::Toolchain;
use crate::Result;

#[derive(Debug, Clone)]
pub struct BuiltDep {
    pub dep_key: String,
    pub config_hash: String,
    /// <artifact entry>/install — the prefix find_package/probe consumes.
    pub install_dir: PathBuf,
}

pub struct DepBuildRequest<'a> {
    pub dep_key: &'a str,
    pub spec: &'a DependencySpec,
    /// Raw store source tree.
    pub source_dir: &'a Path,
    /// Precomputed by the orchestrator (hashing::config_hash).
    pub config_hash: &'a str,
    /// Artifact store entry dir (build happens in <entry>/build-tmp, install
    /// into <entry>/install; caller marks complete).
    pub entry_dir: &'a Path,
    pub config: BuildConfig,
    pub toolchain: &'a Toolchain,
    pub abi_flags: &'a [String],
    /// Install dirs of the transitive needs closure, topo order.
    pub prefix_path: &'a [PathBuf],
}

/// Configure + build + install one dependency. Skips nothing: the caller
/// checks store completeness before calling.
pub fn build_dependency(req: &DepBuildRequest) -> Result<BuiltDep> {
    let _ = req;
    todo!()
}

/// Write the generated toolchain file into `dir`, returning its path.
/// Deterministic content for identical inputs.
pub fn write_toolchain_file(
    dir: &Path,
    toolchain: &Toolchain,
    abi_flags: &[String],
) -> Result<PathBuf> {
    let _ = (dir, toolchain, abi_flags);
    todo!()
}

/// The scrubbed environment used for every cmake/ninja child process.
pub fn scrubbed_env() -> BTreeMap<String, String> {
    todo!()
}
