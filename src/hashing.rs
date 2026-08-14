//! Config hashing for the artifact store (CPP_PKG_IMPLEMENTATION.md §3).
//! blake3 over a canonical serialization; hex-encoded, truncated to 32 chars.

use std::collections::BTreeMap;

/// Everything that makes two builds of the same package non-interchangeable.
///
/// Hash-impact discipline (wave-1 spec §8): the fields below are the COMPLETE
/// set of config-hash inputs. Target flags, non-ABI `[flags]`, cfg
/// conditionals, dev/test markers, `[generate]`, and install/export metadata
/// deliberately have no field here — they must never re-key store artifacts.
/// Patches enter through `package_id` (compose_patched_id), never as a field;
/// system deps enter through `dep_hashes` (sysdep_hash); `Threads::Threads`
/// rides the already-hashed `toolchain` identity.
#[derive(Debug, Clone)]
pub struct ConfigHashInputs<'a> {
    /// Raw-content identity: git commit sha, or "blake3:<hex>" for url deps.
    /// For patched deps this is the COMPOSED id
    /// ("<base>+patches:<hex>", see compose_patched_id) — patched sources
    /// are different sources, so they re-key here rather than via a new
    /// field, leaving the encoding of unpatched entries untouched.
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
    /// System deps contribute their `cppkg-sysdep-v1` hash here through the
    /// same plumbing, making downstream entries machine-local by construction.
    pub dep_hashes: &'a BTreeMap<String, String>,
    /// Configure root inside the checkout (tool-fix A.5), hashed as a literal
    /// string. `None` MUST encode byte-identically to the pre-subdir (v0)
    /// layout — this conditional suffix is the one sanctioned encoding delta
    /// of wave 1, chosen so no existing store entry re-keys. `Some` appends a
    /// domain-tagged suffix after all v1 fields (see config_hash).
    pub subdir: Option<&'a str>,
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
    // Conditional suffix, NOT an unconditional Option encoding: emitting even
    // a "not present" byte for None would re-key every existing store entry,
    // which spec §8 forbids. Appending after all v1 fields cannot collide
    // with a v1 encoding either — the suffix always begins with the
    // length-prefixed domain tag, so any v1 message it could be confused
    // with would need "cppkg-subdir-v1" spliced into dep_hashes, whose own
    // length prefixes pin its extent.
    if let Some(subdir) = inputs.subdir {
        put_str(&mut buf, "cppkg-subdir-v1");
        put_str(&mut buf, subdir);
    }

    let hash = blake3::hash(&buf);
    let mut hex = hash.to_hex().to_string();
    hex.truncate(32);
    hex
}

/// §5.2: identity of an ordered patch set — blake3 over
/// (u64-LE(byte length) || raw bytes) per patch, in application order,
/// truncated to 32 hex chars. Bytes are hashed, not file names, so renaming
/// a patch file never re-keys; reordering or editing one always does.
pub fn patch_set_hash(patches: &[Vec<u8>]) -> String {
    let mut buf = Vec::new();
    for patch in patches {
        buf.extend_from_slice(&(patch.len() as u64).to_le_bytes());
        buf.extend_from_slice(patch);
    }
    let mut hex = blake3::hash(&buf).to_hex().to_string();
    hex.truncate(32);
    hex
}

/// §5.2 hash spine: patched sources are DIFFERENT sources, so patches fold
/// into the package id itself — `<base>+patches:<hex32>` — leaving the
/// `cppkg-config-hash-v1` encoding untouched and every unpatched store
/// entry keyed exactly as before. An empty patch set composes to the base
/// id unchanged (declaring `patches = []` is the same as not declaring it).
pub fn compose_patched_id(base_id: &str, patches: &[Vec<u8>]) -> String {
    if patches.is_empty() {
        return base_id.to_string();
    }
    format!("{base_id}+patches:{}", patch_set_hash(patches))
}

/// §5.3: machine facts that identify a resolved system dependency. All
/// list fields are caller-sorted; `library_hashes[i]` is the blake3 label of
/// the bytes of `library_paths[i]` (same order — do not sort independently).
#[derive(Debug, Clone)]
pub struct SysdepHashInputs<'a> {
    /// Dependency key from the manifest.
    pub key: &'a str,
    /// "cmake" in v1 (the reserved pkg-config mode gets a distinct value).
    pub resolution_mode: &'a str,
    /// Version string the probe resolved on this machine.
    pub resolved_version: &'a str,
    /// Sorted absolute paths of the resolved libraries.
    pub library_paths: &'a [String],
    /// blake3 of each library file's bytes, parallel to `library_paths`.
    pub library_hashes: &'a [String],
    /// Sorted include directories. Header trees are NOT content-hashed —
    /// documented gap accepted by the spec (§5.3).
    pub include_dirs: &'a [String],
}

/// §5.3 sysdep hash — new domain tag "cppkg-sysdep-v1", same canonical
/// length-prefixed encoding and 32-hex truncation as config_hash. The result
/// enters dependents' `dep_hashes` via the existing plumbing, which is what
/// makes store entries downstream of a system dep machine-local by
/// construction (an OS update to the library changes this hash, which
/// changes every dependent's config hash).
pub fn sysdep_hash(inputs: &SysdepHashInputs) -> String {
    let mut buf = Vec::new();
    put_str(&mut buf, "cppkg-sysdep-v1");
    put_str(&mut buf, inputs.key);
    put_str(&mut buf, inputs.resolution_mode);
    put_str(&mut buf, inputs.resolved_version);
    put_list(&mut buf, inputs.library_paths);
    put_list(&mut buf, inputs.library_hashes);
    put_list(&mut buf, inputs.include_dirs);

    let mut hex = blake3::hash(&buf).to_hex().to_string();
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
            subdir: None,
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

    // ---- wave-1 hash-discipline tests (spec §8; plan bundle 5) ----

    /// MANDATORY golden pins: these exact hex values were captured from the
    /// v0 encoder BEFORE wave 1 touched this file. If either assertion ever
    /// fails, the encoding changed and every user store is silently
    /// invalidated — that is a spec §8 violation, not a test to update.
    #[test]
    fn storehash_v0_golden_values_are_pinned() {
        let options = map(&[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("SPDLOG_FMT_EXTERNAL", "ON"),
        ]);
        let abi_flags = vec![
            "-D_GLIBCXX_ASSERTIONS".to_string(),
            "-ffp-contract=off".to_string(),
        ];
        let deps = map(&[("fmt", "00112233445566778899aabbccddeeff")]);
        let i = ConfigHashInputs {
            package_id: "0123456789abcdef0123456789abcdef01234567",
            options: &options,
            build_type: "Release",
            toolchain: "clang;apple;21.0.0;arm64-apple-darwin;libc++",
            abi_flags: &abi_flags,
            dep_hashes: &deps,
            subdir: None,
        };
        assert_eq!(config_hash(&i), "bd1f9951187c4a11c2aeed5bf93d1d23");

        let empty_opts = map(&[]);
        let empty_deps = map(&[]);
        let j = ConfigHashInputs {
            package_id: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            options: &empty_opts,
            build_type: "Debug",
            toolchain: "gcc;gnu;16.0.0;x86_64-linux-gnu;libstdc++",
            abi_flags: &[],
            dep_hashes: &empty_deps,
            subdir: None,
        };
        assert_eq!(config_hash(&j), "500cdfaeff6e71ef9514fdd275dd44f6");
    }

    #[test]
    fn storehash_subdir_some_rekeys_none_does_not() {
        let options = map(&[("A", "ON")]);
        let deps = map(&[("fmt", "cafe")]);
        let abi: Vec<String> = vec![];
        let none = config_hash(&base_inputs(&options, &abi, &deps));

        let mut i = base_inputs(&options, &abi, &deps);
        i.subdir = Some("build/cmake");
        let some = config_hash(&i);
        assert_ne!(none, some);

        // Different subdirs are different builds; even the empty string is
        // distinct from absence (the tag bytes alone re-key).
        let mut j = base_inputs(&options, &abi, &deps);
        j.subdir = Some("build");
        assert_ne!(config_hash(&j), some);
        let mut k = base_inputs(&options, &abi, &deps);
        k.subdir = Some("");
        assert_ne!(config_hash(&k), none);
    }

    #[test]
    fn storehash_subdir_suffix_cannot_masquerade_as_dep_hash() {
        // A dep_hashes entry spelling out the suffix tag must not collide
        // with the genuine subdir encoding: the map's element count pins
        // its extent before the suffix begins.
        let abi: Vec<String> = vec![];
        let opts = map(&[]);
        let deps_with_fake = map(&[("cppkg-subdir-v1", "build/cmake")]);
        let honest_deps = map(&[]);
        let mut real = base_inputs(&opts, &abi, &honest_deps);
        real.subdir = Some("build/cmake");
        let fake = base_inputs(&opts, &abi, &deps_with_fake);
        assert_ne!(config_hash(&real), config_hash(&fake));
    }

    #[test]
    fn storehash_patch_set_hash_is_order_and_content_sensitive() {
        let a = b"patch-a".to_vec();
        let b = b"patch-b".to_vec();
        let h_ab = patch_set_hash(&[a.clone(), b.clone()]);
        let h_ba = patch_set_hash(&[b.clone(), a.clone()]);
        assert_eq!(h_ab.len(), 32);
        assert!(h_ab.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Order is application order — reordering re-keys.
        assert_ne!(h_ab, h_ba);
        // Length prefixes pin patch boundaries.
        let h1 = patch_set_hash(&[b"ab".to_vec(), b"c".to_vec()]);
        let h2 = patch_set_hash(&[b"a".to_vec(), b"bc".to_vec()]);
        assert_ne!(h1, h2);
        // Stable across runs/platforms (patched raw dirs must agree between
        // machines — the fee068f7/f4632513 split this feature fixes).
        assert_eq!(
            patch_set_hash(&[b"hello".to_vec()]),
            patch_set_hash(&[b"hello".to_vec()])
        );
    }

    #[test]
    fn storehash_compose_patched_id_format_and_empty_passthrough() {
        let base = "0123456789abcdef0123456789abcdef01234567";
        // No patches => base id byte-identical (unpatched entries never
        // re-key, per the §8 table).
        assert_eq!(compose_patched_id(base, &[]), base);

        let patches = vec![b"--- a/x\n+++ b/x\n".to_vec()];
        let composed = compose_patched_id(base, &patches);
        let expected_suffix = patch_set_hash(&patches);
        assert_eq!(composed, format!("{base}+patches:{expected_suffix}"));

        // Same bytes under any file name => same id (bytes are hashed,
        // not names — renaming a patch file must not re-key).
        assert_eq!(composed, compose_patched_id(base, &patches.clone()));
    }

    fn sysdep_base<'a>(
        library_paths: &'a [String],
        library_hashes: &'a [String],
        include_dirs: &'a [String],
    ) -> SysdepHashInputs<'a> {
        SysdepHashInputs {
            key: "zstd",
            resolution_mode: "cmake",
            resolved_version: "1.5.6",
            library_paths,
            library_hashes,
            include_dirs,
        }
    }

    #[test]
    fn storehash_sysdep_hash_covers_every_field() {
        let libs = vec!["/usr/lib/libzstd.dylib".to_string()];
        let lib_hashes = vec![blake3_bytes_labeled(b"fake library bytes")];
        let incs = vec!["/usr/include".to_string()];
        let base = sysdep_hash(&sysdep_base(&libs, &lib_hashes, &incs));
        assert_eq!(base.len(), 32);
        assert!(base.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let mut i = sysdep_base(&libs, &lib_hashes, &incs);
        i.key = "boost";
        assert_ne!(sysdep_hash(&i), base);

        let mut i = sysdep_base(&libs, &lib_hashes, &incs);
        i.resolution_mode = "pkg-config";
        assert_ne!(sysdep_hash(&i), base);

        let mut i = sysdep_base(&libs, &lib_hashes, &incs);
        i.resolved_version = "1.5.7";
        assert_ne!(sysdep_hash(&i), base);

        let libs2 = vec!["/opt/lib/libzstd.dylib".to_string()];
        assert_ne!(sysdep_hash(&sysdep_base(&libs2, &lib_hashes, &incs)), base);

        // The OS updating the library file (same path, new bytes) re-keys —
        // this is the machine-locality guarantee of §5.3.
        let lib_hashes2 = vec![blake3_bytes_labeled(b"updated library bytes")];
        assert_ne!(sysdep_hash(&sysdep_base(&libs, &lib_hashes2, &incs)), base);

        let incs2 = vec!["/opt/include".to_string()];
        assert_ne!(sysdep_hash(&sysdep_base(&libs, &lib_hashes, &incs2)), base);
    }

    #[test]
    fn storehash_sysdep_hash_is_stable_and_domain_separated() {
        let libs: Vec<String> = vec![];
        let hashes: Vec<String> = vec![];
        let incs: Vec<String> = vec![];
        let a = sysdep_hash(&sysdep_base(&libs, &hashes, &incs));
        let b = sysdep_hash(&sysdep_base(&libs, &hashes, &incs));
        assert_eq!(a, b);

        // List boundaries must not be movable between adjacent list fields.
        let one = vec!["x".to_string()];
        let with_lib = sysdep_hash(&sysdep_base(&one, &hashes, &incs));
        let with_hash = sysdep_hash(&sysdep_base(&libs, &one, &incs));
        let with_inc = sysdep_hash(&sysdep_base(&libs, &hashes, &one));
        assert_ne!(with_lib, with_hash);
        assert_ne!(with_hash, with_inc);
        assert_ne!(with_lib, with_inc);
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

