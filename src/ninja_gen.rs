//! build.ninja + compile_commands.json emission (CPP_PKG_IMPLEMENTATION.md
//! §6). Regenerated unconditionally on every `cpp-pkg build` (v0).
//!
//! Conventions (mirror CMake's battle-tested Ninja output):
//! - One rule per (language, action): cxx_compile, c_compile, archive, link.
//! - Header deps: `deps = gcc` + `-MD -MT $out -MF $out.d` (GNU dialect).
//! - Per-unit flags in build statements (via per-build variables), not rules.
//! - Archive rule must `rm -f $out` first (ar appends otherwise).
//! - Paths with spaces: ninja `$ `-escaping ('$ ', '$:', '$$').
//! - compile_commands.json: one entry per CompileUnit, "arguments" array
//!   form (not "command" string), directory = build dir. Feeds --query.

use std::path::Path;

use crate::graph::BuildPlan;
use crate::schema::BuildConfig;
use crate::toolchain::{Driver, Toolchain};
use crate::Result;

/// Write `<build_dir>/build.ninja`.
pub fn write_ninja(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
    build_dir: &Path,
) -> Result<()> {
    let _ = (plan, toolchain, driver, config, build_dir);
    todo!()
}

/// Write `<build_dir>/compile_commands.json` for the same plan.
pub fn write_compile_commands(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
    build_dir: &Path,
) -> Result<()> {
    let _ = (plan, toolchain, driver, config, build_dir);
    todo!()
}

/// Run ninja in `build_dir` (scrubbed env, cmake_build::scrubbed_env),
/// forwarding stdout/stderr to the user. Nonzero exit -> error.
pub fn run_ninja(build_dir: &Path, targets: &[String]) -> Result<()> {
    let _ = (build_dir, targets);
    todo!()
}
