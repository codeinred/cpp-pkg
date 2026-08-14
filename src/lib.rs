//! cpp-pkg: a C++ package manager / build system consuming CMake dependencies.
//!
//! Normative design documents (read before modifying anything):
//! - CPP_PKG.md            — concept
//! - CPP_PKG_IMPLEMENTATION.md — strategic decisions (numbered sections)
//! - CPPKG_TOML.md         — CppPkg.toml + CppPkg.lock schema (normative)
//! - DESIGN_CHOICES.md     — fine-grained decision log
//!
//! Module map (one implementation agent per bundle; do not edit outside your
//! bundle — report contract mismatches instead of fixing other modules):
//!   schema, lockfile      — CppPkg.toml / CppPkg.lock parse + validate
//!   toolchain             — detection, semantic identity, GNU flag driver
//!   hashing, store, fetch — config hash, on-disk stores, git/url acquisition
//!   cmake_build           — dependency configure/build/install via CMake
//!   probe                 — tier-2 extraction probe project + record parser
//!   manifest              — CPS-style manifest types, probe → manifest
//!   graph                 — name resolution, visibility propagation, plans
//!   ninja_gen             — build.ninja + compile_commands.json emission
//!   shim                  — Config.cmake shim + dependency provider script
//!   cli                   — clap CLI + build orchestration (integration)

pub mod cli;
pub mod cmake_build;
pub mod fetch;
pub mod graph;
pub mod hashing;
pub mod interp;
pub mod lockfile;
pub mod manifest;
pub mod ninja_gen;
pub mod probe;
pub mod schema;
pub mod shim;
pub mod store;
pub mod toolchain;

pub type Result<T> = anyhow::Result<T>;

/// Schema version stamped into CppPkg.toml, CppPkg.lock, manifests, and
/// store entry marker files (decided: format versioning from day one).
pub const SCHEMA_VERSION: u32 = 1;
