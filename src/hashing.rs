//! Config hashing for the artifact store (CPP_PKG_IMPLEMENTATION.md §3).
//! blake3 over a canonical serialization; hex-encoded, truncated to 32 chars.

use std::collections::BTreeMap;

/// Everything that makes two builds of the same package non-interchangeable.
#[derive(Debug, Clone)]
pub struct ConfigHashInputs<'a> {
    /// Raw-content identity: git commit sha, or "blake3:<hex>" for url deps.
    pub package_id: &'a str,
    /// CMake cache options, LITERAL strings (never normalized: ON != TRUE
    /// != 1 by design — see CPPKG_TOML.md).
    pub options: &'a BTreeMap<String, String>,
    /// CMake config name ("Debug", ...).
    pub build_type: &'a str,
    /// toolchain::ToolchainIdentity::hash_input()
    pub toolchain: &'a str,
    /// ABI-classified profile flags, in profile order (these reach the dep
    /// build via the generated toolchain file).
    pub abi_flags: &'a [String],
    /// Config hashes of this dep's `needs` (direct is sufficient: each dep's
    /// hash already folds in ITS needs — Nix-derivation-style transitivity).
    pub dep_hashes: &'a BTreeMap<String, String>,
}

/// Canonical, unambiguous encoding (length-prefixed or escaped fields — no
/// separator-injection ambiguity) -> blake3 -> first 32 hex chars.
/// Changing the encoding invalidates every store entry: version it via the
/// store entry marker (store::SCHEMA_VERSION).
pub fn config_hash(inputs: &ConfigHashInputs) -> String {
    let _ = inputs;
    todo!()
}

/// blake3 of a byte stream (used for url-dependency archive bytes),
/// full hex, rendered "blake3:<hex>".
pub fn blake3_bytes_labeled(bytes: &[u8]) -> String {
    let _ = bytes;
    todo!()
}
