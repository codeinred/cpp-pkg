//! CLI + build orchestration (per CPP_PKG.md CLI section).
//!
//! v0 surface:
//!   cpp-pkg build [TARGETS...] [--config <debug|release|relwithdebinfo|
//!     minsizerel>] [--toolchain <name-or-path>] [--query [PATH]]
//!   cpp-pkg provide --package <find-name> --project <dir> ... (internal;
//!     used by the dependency provider shim)
//!
//! Orchestration pipeline for `build` (each step's module owns its logic —
//! this function only sequences and reports):
//!   1. schema::load(CppPkg.toml) -> print Warnings
//!   2. toolchain: --toolchain path/preset or detect_default()
//!   3. lockfile::load; stores::open_default + lock
//!   4. schema::dependency_build_order; for each dep:
//!        fetch::ensure (with lock entry) -> update lockfile entry
//!        hashing::config_hash (needs' hashes from earlier iterations)
//!        if !store.entry_complete: cmake_build::build_dependency,
//!          probe::probe_installed, manifest::from_probe + save,
//!          store.mark_complete
//!        else: manifest::load
//!   5. lockfile::save (only if changed)
//!   6. graph::plan -> ninja_gen::write_ninja + write_compile_commands
//!   7. --query: print compile commands (all targets or the given TU) and
//!      exit without building; otherwise ninja_gen::run_ninja
//!
//! `--path <file> --with <dep>...` fast-prototyping flow: DEFERRED post-v0
//! (schema-adjacent CLI recorded in DESIGN_CHOICES.md Open).

use crate::Result;

pub fn run() -> Result<()> {
    todo!()
}
