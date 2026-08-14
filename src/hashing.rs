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

// Canonical encoding primitives. Every variable-length item is preceded by
// its byte length (u64 LE), and every collection by its element count, so no
// choice of field contents can collide with a different field layout
// ("ab"+"c" vs "a"+"bc", or a value migrating between adjacent fields).
fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn put_map(buf: &mut Vec<u8>, map: &BTreeMap<String, String>) {
    // BTreeMap iteration is key-sorted, making the encoding independent of
    // insertion order by construction.
    buf.extend_from_slice(&(map.len() as u64).to_le_bytes());
    for (k, v) in map {
        put_str(buf, k);
        put_str(buf, v);
    }
}

fn put_list(buf: &mut Vec<u8>, items: &[String]) {
    buf.extend_from_slice(&(items.len() as u64).to_le_bytes());
    for item in items {
        put_str(buf, item);
    }
}

/// Canonical, unambiguous encoding (length-prefixed or escaped fields — no
/// separator-injection ambiguity) -> blake3 -> first 32 hex chars.
/// Changing the encoding invalidates every store entry: version it via the
/// store entry marker (store::SCHEMA_VERSION).
pub fn config_hash(inputs: &ConfigHashInputs) -> String {
    let mut buf = Vec::new();
    // Domain tag + encoding version: bump the number if the field set or
    // encoding ever changes, so old and new hashes can never collide.
    put_str(&mut buf, "cppkg-config-hash-v1");
    put_str(&mut buf, inputs.package_id);
    put_map(&mut buf, inputs.options);
    put_str(&mut buf, inputs.build_type);
    put_str(&mut buf, inputs.toolchain);
    put_list(&mut buf, inputs.abi_flags);
    put_map(&mut buf, inputs.dep_hashes);

    let hash = blake3::hash(&buf);
    let mut hex = hash.to_hex().to_string();
    hex.truncate(32);
    hex
}

/// blake3 of a byte stream (used for url-dependency archive bytes),
/// full hex, rendered "blake3:<hex>".
pub fn blake3_bytes_labeled(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn base_inputs<'a>(
        options: &'a BTreeMap<String, String>,
        abi_flags: &'a [String],
        dep_hashes: &'a BTreeMap<String, String>,
    ) -> ConfigHashInputs<'a> {
        ConfigHashInputs {
            package_id: "0123456789abcdef0123456789abcdef01234567",
            options,
            build_type: "Release",
            toolchain: "clang;apple;21.0.0;arm64-apple-darwin;libc++",
            abi_flags,
            dep_hashes,
        }
    }

    #[test]
    fn storehash_hash_is_32_lowercase_hex_chars() {
        let options = map(&[]);
        let deps = map(&[]);
        let h = config_hash(&base_inputs(&options, &[], &deps));
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn storehash_map_insertion_order_is_irrelevant() {
        // Same logical maps, opposite insertion orders.
        let mut opts_a = BTreeMap::new();
        opts_a.insert("SPDLOG_FMT_EXTERNAL".to_string(), "ON".to_string());
        opts_a.insert("BUILD_SHARED_LIBS".to_string(), "OFF".to_string());
        let mut opts_b = BTreeMap::new();
        opts_b.insert("BUILD_SHARED_LIBS".to_string(), "OFF".to_string());
        opts_b.insert("SPDLOG_FMT_EXTERNAL".to_string(), "ON".to_string());

        let mut deps_a = BTreeMap::new();
        deps_a.insert("fmt".to_string(), "aaaa".to_string());
        deps_a.insert("zlib".to_string(), "bbbb".to_string());
        let mut deps_b = BTreeMap::new();
        deps_b.insert("zlib".to_string(), "bbbb".to_string());
        deps_b.insert("fmt".to_string(), "aaaa".to_string());

        let h_a = config_hash(&base_inputs(&opts_a, &[], &deps_a));
        let h_b = config_hash(&base_inputs(&opts_b, &[], &deps_b));
        assert_eq!(h_a, h_b);
    }

    #[test]
    fn storehash_every_field_affects_the_hash() {
        let options = map(&[("A", "ON")]);
        let abi_flags = vec!["-D_GLIBCXX_ASSERTIONS".to_string()];
        let deps = map(&[("fmt", "cafe")]);
        let base = config_hash(&base_inputs(&options, &abi_flags, &deps));

        // package_id
        let mut i = base_inputs(&options, &abi_flags, &deps);
        i.package_id = "fedcba9876543210fedcba9876543210fedcba98";
        assert_ne!(config_hash(&i), base);

        // options (value change)
        let options2 = map(&[("A", "TRUE")]);
        let i = base_inputs(&options2, &abi_flags, &deps);
        assert_ne!(config_hash(&i), base);

        // build_type
        let mut i = base_inputs(&options, &abi_flags, &deps);
        i.build_type = "Debug";
        assert_ne!(config_hash(&i), base);

        // toolchain
        let mut i = base_inputs(&options, &abi_flags, &deps);
        i.toolchain = "gcc;gnu;15.1.0;arm64-apple-darwin;libstdc++";
        assert_ne!(config_hash(&i), base);

        // abi_flags
        let abi_flags2 = vec!["-D_GLIBCXX_DEBUG".to_string()];
        let i = base_inputs(&options, &abi_flags2, &deps);
        assert_ne!(config_hash(&i), base);

        // dep_hashes
        let deps2 = map(&[("fmt", "beef")]);
        let i = base_inputs(&options, &abi_flags, &deps2);
        assert_ne!(config_hash(&i), base);
    }

    #[test]
    fn storehash_options_are_literal_never_normalized() {
        let abi: Vec<String> = vec![];
        let deps = map(&[]);
        let on = config_hash(&base_inputs(&map(&[("X", "ON")]), &abi, &deps));
        let tru = config_hash(&base_inputs(&map(&[("X", "TRUE")]), &abi, &deps));
        let one = config_hash(&base_inputs(&map(&[("X", "1")]), &abi, &deps));
        assert_ne!(on, tru);
        assert_ne!(on, one);
        assert_ne!(tru, one);
    }

    #[test]
    fn storehash_no_separator_injection_ambiguity() {
        let abi: Vec<String> = vec![];
        let deps = map(&[]);

        // Key/value boundary must not be movable.
        let a = config_hash(&base_inputs(&map(&[("AB", "C")]), &abi, &deps));
        let b = config_hash(&base_inputs(&map(&[("A", "BC")]), &abi, &deps));
        assert_ne!(a, b);

        // List element boundaries must not be movable.
        let opts = map(&[]);
        let one_flag = vec!["-a-b".to_string()];
        let two_flags = vec!["-a".to_string(), "-b".to_string()];
        let h1 = config_hash(&base_inputs(&opts, &one_flag, &deps));
        let h2 = config_hash(&base_inputs(&opts, &two_flags, &deps));
        assert_ne!(h1, h2);

        // Content must not be movable across adjacent scalar fields.
        let mut i = base_inputs(&opts, &[], &deps);
        i.build_type = "ReleaseX";
        i.toolchain = "clang";
        let mut j = base_inputs(&opts, &[], &deps);
        j.build_type = "Release";
        j.toolchain = "Xclang";
        assert_ne!(config_hash(&i), config_hash(&j));
    }

    #[test]
    fn storehash_abi_flag_order_is_significant() {
        // Profile order is meaningful for flags (later flags can override
        // earlier ones), so reordering must change the hash.
        let opts = map(&[]);
        let deps = map(&[]);
        let ab = vec!["-a".to_string(), "-b".to_string()];
        let ba = vec!["-b".to_string(), "-a".to_string()];
        assert_ne!(
            config_hash(&base_inputs(&opts, &ab, &deps)),
            config_hash(&base_inputs(&opts, &ba, &deps))
        );
    }

    #[test]
    fn storehash_blake3_bytes_labeled_format() {
        let h = blake3_bytes_labeled(b"hello");
        assert!(h.starts_with("blake3:"));
        // blake3 output is 32 bytes = 64 hex chars.
        assert_eq!(h.len(), "blake3:".len() + 64);
        // Known-answer: stable across runs and platforms.
        assert_eq!(
            h,
            "blake3:ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f"
        );
        // Distinct inputs produce distinct labels.
        assert_ne!(blake3_bytes_labeled(b"hello"), blake3_bytes_labeled(b"hellp"));
    }
}
