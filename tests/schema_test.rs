//! Unit tests for cppkg::schema. Kept as an integration-test target (not
//! inline #[cfg(test)]) so they compile independently of sibling modules'
//! in-flight #[cfg(test)] code; everything exercised is public API.

use cppkg::schema::*;
use std::collections::BTreeMap;


fn ok(text: &str) -> (ProjectFile, Warnings) {
    parse_str(text).expect("expected manifest to parse")
}

fn err(text: &str) -> String {
    format!("{:#}", parse_str(text).expect_err("expected manifest to fail"))
}

const MINIMAL: &str = "schema-version = 1\n[package]\nname = \"app\"\n";

fn with_dep(dep_toml: &str) -> String {
    format!("{MINIMAL}\n[dependencies]\n{dep_toml}\n")
}

// -- BuildConfig ------------------------------------------------------

#[test]
fn schema_build_config_conversions() {
    for cfg in [
        BuildConfig::Debug,
        BuildConfig::Release,
        BuildConfig::RelWithDebInfo,
        BuildConfig::MinSizeRel,
    ] {
        assert_eq!(BuildConfig::from_key(cfg.key()).unwrap(), cfg);
        assert_eq!(cfg.cmake_name().to_lowercase(), cfg.key());
    }
    assert_eq!(BuildConfig::Debug.cmake_name(), "Debug");
    assert_eq!(BuildConfig::RelWithDebInfo.cmake_name(), "RelWithDebInfo");
    let e = format!("{:#}", BuildConfig::from_key("Debug").unwrap_err());
    assert!(e.contains("relwithdebinfo"), "should list valid keys: {e}");
}

// -- Minimal + full parse --------------------------------------------

#[test]
fn schema_minimal_manifest_parses() {
    let (p, w) = ok(MINIMAL);
    assert_eq!(p.package.name, "app");
    assert!(p.package.version.is_none());
    assert!(p.dependencies.is_empty() && p.targets.is_empty());
    assert!(w.0.is_empty());
}

#[test]
fn schema_annotated_example_parses() {
    let text = r#"
schema-version = 1

[package]
name = "myapp"
version = "0.1.0"

[toolchains.gcc-homebrew]
cxx = "g++-15"
cc  = "gcc-15"
ar  = "gcc-ar-15"

[profiles.debug]
cxx-flags  = ["-fsanitize=address"]
c-flags    = []
link-flags = ["-fsanitize=address"]

[dependencies]
fmt    = { git = "https://github.com/fmtlib/fmt", tag = "11.2.0" }
# NOTE: CPPKG_TOML.md's annotated example wraps these inline tables across
# lines, which TOML 1.0 forbids — standard table syntax here instead.
[dependencies.spdlog]
git = "https://github.com/gabime/spdlog"
tag = "v1.15.3"
options = { SPDLOG_FMT_EXTERNAL = "ON" }
needs = ["fmt"]

[dependencies.zlib]
url = "https://zlib.net/zlib-1.3.1.tar.gz"
sha256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"

[targets.core]
type    = "static-library"
sources = ["src/core/**/*.cpp"]
cxx-std = 20
includes = { public = ["include"], private = ["src"] }
defines  = { public = ["CORE_API="], private = ["CORE_INTERNAL"] }
dependencies = { public = ["fmt::fmt"], private = ["spdlog::spdlog"] }

[targets.myapp]
type    = "executable"
sources = ["src/main.cpp"]
dependencies = ["core"]
"#;
    let (p, w) = ok(text);
    assert_eq!(p.package.version.as_deref(), Some("0.1.0"));
    assert_eq!(p.toolchains["gcc-homebrew"].cxx, "g++-15");
    assert_eq!(p.toolchains["gcc-homebrew"].ar.as_deref(), Some("gcc-ar-15"));
    assert_eq!(p.profiles["debug"].cxx_flags, vec!["-fsanitize=address"]);

    let spdlog = &p.dependencies["spdlog"];
    assert_eq!(spdlog.needs, vec!["fmt"]);
    assert_eq!(spdlog.options["SPDLOG_FMT_EXTERNAL"], "ON");
    match &p.dependencies["fmt"].source {
        SourceSpec::Git { url, reference: GitRef::Tag(t) } => {
            assert_eq!(url, "https://github.com/fmtlib/fmt");
            assert_eq!(t, "11.2.0");
        }
        other => panic!("wrong fmt source: {other:?}"),
    }
    match &p.dependencies["zlib"].source {
        SourceSpec::Url { sha256, .. } => assert_eq!(sha256.len(), 64),
        other => panic!("wrong zlib source: {other:?}"),
    }

    let core = &p.targets["core"];
    assert_eq!(core.kind, TargetKind::StaticLibrary);
    assert_eq!(core.cxx_std, Some(20));
    assert_eq!(core.includes.public, vec!["include"]);
    assert_eq!(core.includes.private, vec!["src"]);
    assert_eq!(core.dependencies.public, vec!["fmt::fmt"]);

    let myapp = &p.targets["myapp"];
    assert_eq!(myapp.kind, TargetKind::Executable);
    // Bare-list sugar => all private.
    assert!(myapp.dependencies.public.is_empty());
    assert_eq!(myapp.dependencies.private, vec!["core"]);

    // Sanitizer flags in the debug profile => warnings (cxx + link).
    assert_eq!(w.0.len(), 2, "warnings: {:?}", w.0);
    assert!(w.0[0].contains("uninstrumented"));
}

// -- VisibilitySplit sugar -------------------------------------------

#[test]
fn schema_bare_list_is_private_on_all_three_fields() {
    let text = format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         includes = [\"inc\"]\ndefines = [\"D=1\"]\ndependencies = [\"x\"]\n"
    );
    let (p, _) = ok(&text);
    let t = &p.targets["t"];
    for (split, want) in [
        (&t.includes, "inc"),
        (&t.defines, "D=1"),
        (&t.dependencies, "x"),
    ] {
        assert!(split.public.is_empty());
        assert_eq!(split.private, vec![want]);
    }
}

#[test]
fn schema_visibility_table_form_and_partial_table() {
    let text = format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         includes = {{ public = [\"include\"] }}\n"
    );
    let (p, _) = ok(&text);
    let t = &p.targets["t"];
    assert_eq!(t.includes.public, vec!["include"]);
    assert!(t.includes.private.is_empty());
    // Omitted fields default to empty splits.
    assert!(t.defines.public.is_empty() && t.defines.private.is_empty());
}

#[test]
fn schema_visibility_unknown_key_rejected() {
    let text = format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         includes = {{ public = [], interface = [] }}\n"
    );
    parse_str(&text).expect_err("interface bucket is deferred, must reject");
}

// -- Dependency sources ----------------------------------------------

#[test]
fn schema_source_git_rev_form() {
    let (p, _) = ok(&with_dep(
        "d = { git = \"https://x/y\", rev = \"abc123\" }",
    ));
    match &p.dependencies["d"].source {
        SourceSpec::Git { reference: GitRef::Rev(r), .. } => assert_eq!(r, "abc123"),
        other => panic!("wrong source: {other:?}"),
    }
}

#[test]
fn schema_source_git_and_url_is_ambiguous() {
    let e = err(&with_dep(
        "d = { git = \"https://x\", tag = \"1\", url = \"https://y\", \
         sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\" }",
    ));
    assert!(e.contains("exactly one"), "message should say exactly one: {e}");
    assert!(e.contains("'d'"), "message should name the dep: {e}");
}

#[test]
fn schema_source_missing_entirely() {
    let e = err(&with_dep("d = { needs = [] }"));
    assert!(e.contains("no source"), "{e}");
}

#[test]
fn schema_source_git_needs_tag_or_rev() {
    let e = err(&with_dep("d = { git = \"https://x\" }"));
    assert!(e.contains("tag") && e.contains("rev"), "{e}");
}

#[test]
fn schema_source_git_tag_and_rev_conflict() {
    let e = err(&with_dep("d = { git = \"https://x\", tag = \"1\", rev = \"a1\" }"));
    assert!(e.contains("both `tag` and `rev`"), "{e}");
}

#[test]
fn schema_source_url_needs_sha256() {
    let e = err(&with_dep("d = { url = \"https://x/a.tar.gz\" }"));
    assert!(e.contains("sha256"), "{e}");
}

#[test]
fn schema_source_url_rejects_git_ref_fields() {
    let e = err(&with_dep(
        "d = { url = \"https://x/a.tar.gz\", tag = \"1\", \
         sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\" }",
    ));
    assert!(e.contains("only apply to `git`"), "{e}");
}

#[test]
fn schema_source_git_rejects_sha256() {
    let e = err(&with_dep("d = { git = \"https://x\", tag = \"1\", sha256 = \"aa\" }"));
    assert!(e.contains("sha256"), "{e}");
}

#[test]
fn schema_source_sha256_must_be_64_hex() {
    let e = err(&with_dep("d = { url = \"https://x\", sha256 = \"abc\" }"));
    assert!(e.contains("64 hex"), "{e}");
    let e = err(&with_dep(&format!(
        "d = {{ url = \"https://x\", sha256 = \"{}\" }}",
        "g".repeat(64)
    )));
    assert!(e.contains("64 hex"), "{e}");
}

// -- ExposesTargets ---------------------------------------------------

#[test]
fn schema_exposes_targets_list_and_map_forms() {
    let (p, _) = ok(&with_dep(
        "a = { git = \"https://x\", tag = \"1\", exposes-targets = [\"fmt::fmt\"] }\n\
         b = { git = \"https://y\", tag = \"1\", exposes-targets = { \"fmt::fmt\" = \"fmt\" } }",
    ));
    let a = &p.dependencies["a"].exposes_targets;
    assert_eq!(a.claims, vec!["fmt::fmt"]);
    assert!(a.renames.is_empty());
    let b = &p.dependencies["b"].exposes_targets;
    assert_eq!(b.claims, vec!["fmt::fmt"]);
    assert_eq!(b.renames["fmt::fmt"], "fmt");
}

#[test]
fn schema_exposes_namespace_parses() {
    let (p, _) = ok(&with_dep(
        "a = { git = \"https://x\", tag = \"1\", exposes-namespace = [\"fmt\"] }",
    ));
    assert_eq!(p.dependencies["a"].exposes_namespace, vec!["fmt"]);
}

// -- needs integrity + cycles ----------------------------------------

#[test]
fn schema_needs_unknown_key_is_error() {
    let e = err(&with_dep(
        "a = { git = \"https://x\", tag = \"1\", needs = [\"nope\"] }",
    ));
    assert!(e.contains("'a'") && e.contains("'nope'"), "{e}");
    assert!(e.contains("[dependencies]"), "should point at the fix: {e}");
}

#[test]
fn schema_needs_cycle_error_names_the_cycle() {
    let e = err(&with_dep(
        "a = { git = \"https://x\", tag = \"1\", needs = [\"b\"] }\n\
         b = { git = \"https://y\", tag = \"1\", needs = [\"c\"] }\n\
         c = { git = \"https://z\", tag = \"1\", needs = [\"a\"] }",
    ));
    assert!(e.contains("cycle"), "{e}");
    // The concrete path must appear, arrow-joined, closing the loop.
    assert!(
        e.contains("a -> b -> c -> a")
            || e.contains("b -> c -> a -> b")
            || e.contains("c -> a -> b -> c"),
        "cycle path missing: {e}"
    );
}

#[test]
fn schema_needs_self_cycle() {
    let e = err(&with_dep(
        "a = { git = \"https://x\", tag = \"1\", needs = [\"a\"] }",
    ));
    assert!(e.contains("cycle") && e.contains("a -> a"), "{e}");
}

// -- Build order + closure -------------------------------------------

fn deps_from(text: &str) -> BTreeMap<String, DependencySpec> {
    ok(&with_dep(text)).0.dependencies
}

#[test]
fn schema_build_order_lexicographic_when_unconstrained() {
    let deps = deps_from(
        "c = { git = \"https://x\", tag = \"1\" }\n\
         a = { git = \"https://x\", tag = \"1\" }\n\
         b = { git = \"https://x\", tag = \"1\" }",
    );
    assert_eq!(dependency_build_order(&deps).unwrap(), ["a", "b", "c"]);
}

#[test]
fn schema_build_order_respects_needs_with_stable_tie_break() {
    // z is needed by a, so z must come first despite sorting last;
    // b and c are unconstrained and tie-break lexicographically.
    let deps = deps_from(
        "a = { git = \"https://x\", tag = \"1\", needs = [\"z\"] }\n\
         c = { git = \"https://x\", tag = \"1\" }\n\
         b = { git = \"https://x\", tag = \"1\", needs = [\"z\"] }\n\
         z = { git = \"https://x\", tag = \"1\" }",
    );
    assert_eq!(dependency_build_order(&deps).unwrap(), ["c", "z", "a", "b"]);
}

#[test]
fn schema_build_order_unknown_need_standalone() {
    let mut deps = deps_from("a = { git = \"https://x\", tag = \"1\" }");
    deps.get_mut("a").unwrap().needs.push("ghost".into());
    let e = format!("{:#}", dependency_build_order(&deps).unwrap_err());
    assert!(e.contains("'ghost'"), "{e}");
}

#[test]
fn schema_needs_closure_transitive_and_deduped() {
    // spdlog -> fmt; curl -> {zlib, spdlog}; both paths reach fmt once.
    let deps = deps_from(
        "fmt    = { git = \"https://x\", tag = \"1\" }\n\
         spdlog = { git = \"https://x\", tag = \"1\", needs = [\"fmt\"] }\n\
         zlib   = { git = \"https://x\", tag = \"1\" }\n\
         curl   = { git = \"https://x\", tag = \"1\", needs = [\"zlib\", \"spdlog\", \"zlib\"] }",
    );
    assert_eq!(needs_closure(&deps, "curl").unwrap(), ["fmt", "spdlog", "zlib"]);
    assert_eq!(needs_closure(&deps, "spdlog").unwrap(), ["fmt"]);
    assert!(needs_closure(&deps, "fmt").unwrap().is_empty());
}

#[test]
fn schema_needs_closure_unknown_key() {
    let deps = deps_from("a = { git = \"https://x\", tag = \"1\" }");
    let e = format!("{:#}", needs_closure(&deps, "nope").unwrap_err());
    assert!(e.contains("'nope'"), "{e}");
}

#[test]
fn schema_needs_closure_cycle_detected_standalone() {
    let mut deps = deps_from(
        "a = { git = \"https://x\", tag = \"1\" }\n\
         b = { git = \"https://x\", tag = \"1\", needs = [\"a\"] }",
    );
    deps.get_mut("a").unwrap().needs.push("b".into());
    let e = format!("{:#}", needs_closure(&deps, "a").unwrap_err());
    assert!(e.contains("cycle"), "{e}");
}

// -- Profiles ---------------------------------------------------------

#[test]
fn schema_profile_restricted_to_builtins() {
    let e = err(&format!(
        "{MINIMAL}\n[profiles.debug-asan]\ncxx-flags = []\n"
    ));
    assert!(e.contains("debug-asan"), "{e}");
    assert!(e.contains("base-config"), "should mention the reserved path: {e}");
}

#[test]
fn schema_sanitizer_warning_collected_not_fatal() {
    let (_, w) = ok(&format!(
        "{MINIMAL}\n[profiles.release]\ncxx-flags = [\"-O2\", \"-fsanitize=thread\"]\n"
    ));
    assert_eq!(w.0.len(), 1);
    assert!(w.0[0].contains("-fsanitize=thread") && w.0[0].contains("release"));
}

#[test]
fn schema_abi_flags_are_allowed_without_warning() {
    let (p, w) = ok(&format!(
        "{MINIMAL}\n[profiles.debug]\ncxx-flags = [\"-D_GLIBCXX_DEBUG\", \"-stdlib=libc++\"]\n"
    ));
    assert!(w.0.is_empty(), "ABI flags propagate, no warning: {:?}", w.0);
    assert_eq!(p.profiles["debug"].cxx_flags.len(), 2);
}

// -- Charsets ---------------------------------------------------------

#[test]
fn schema_charset_rejections() {
    let e = err("schema-version = 1\n[package]\nname = \"my app\"\n");
    assert!(e.contains("'my app'"), "{e}");

    let e = err(&with_dep("\"bad::key\" = { git = \"https://x\", tag = \"1\" }"));
    assert!(e.contains("bad::key"), "{e}");

    let e = err(&format!(
        "{MINIMAL}\n[targets.\"a/b\"]\ntype = \"executable\"\nsources = [\"m.cpp\"]\n"
    ));
    assert!(e.contains("a/b"), "{e}");
}

// -- Unknown keys / schema-version / misc -----------------------------

#[test]
fn schema_unknown_keys_rejected_everywhere() {
    // Top level.
    parse_str(&format!("{MINIMAL}\nfrobnicate = 1\n")).unwrap_err();
    // [package].
    parse_str("schema-version = 1\n[package]\nname = \"a\"\nauthor = \"x\"\n").unwrap_err();
    // Dependency table (typo'd field).
    parse_str(&with_dep("d = { git = \"https://x\", tag = \"1\", need = [\"z\"] }"))
        .unwrap_err();
    // Target table. (`link-flags` became legal in wave 1 — use a real typo.)
    parse_str(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = []\nlinker-flags = []\n"
    ))
    .unwrap_err();
    // Toolchain preset.
    parse_str(&format!(
        "{MINIMAL}\n[toolchains.x]\ncxx = \"c++\"\nranlib = \"r\"\n"
    ))
    .unwrap_err();
    // Profile.
    parse_str(&format!(
        "{MINIMAL}\n[profiles.debug]\nld-flags = []\n"
    ))
    .unwrap_err();
}

#[test]
fn schema_version_mismatch() {
    let e = err("schema-version = 99\n[package]\nname = \"a\"\n");
    assert!(e.contains("99"), "{e}");
    let e = err("[package]\nname = \"a\"\n");
    assert!(e.contains("schema-version"), "missing field should be named: {e}");
}

#[test]
fn schema_unknown_target_type() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"shared-library\"\nsources = [\"a.cpp\"]\n"
    ));
    assert!(e.contains("shared-library"), "{e}");
    assert!(e.contains("static-library"), "should list valid kinds: {e}");
}

#[test]
fn schema_load_reads_file_and_reports_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CppPkg.toml");
    std::fs::write(&path, MINIMAL).unwrap();
    let (p, _) = load(&path).unwrap();
    assert_eq!(p.package.name, "app");

    std::fs::write(&path, "schema-version = 99\n[package]\nname = \"a\"\n").unwrap();
    let e = format!("{:#}", load(&path).unwrap_err());
    assert!(e.contains("CppPkg.toml"), "error should carry the path: {e}");

    let e = format!("{:#}", load(&dir.path().join("missing.toml")).unwrap_err());
    assert!(e.contains("missing.toml"), "{e}");
}

// ===========================================================================
// Wave 1 — cfg sub-tables (spec §2)
// ===========================================================================

#[test]
fn schema_cfg_canonical_examples_parse() {
    // The §2.3 normative shapes, verbatim in structure.
    let text = format!(
        r#"{MINIMAL}
[targets.libninja]
type = "static-library"
sources = ["src/build.cc"]

[targets.libninja.cfg.unix]
sources = ["src/jobserver-posix.cc", "src/subprocess-posix.cc"]

[targets.libninja.cfg.linux]
# transcribed: check_cxx_symbol_exists(ppoll ...)
defines = {{ private = ["USE_PPOLL"] }}

[targets.libninja.cfg.windows]
sources = ["src/subprocess-win32.cc", "src/getopt.c"]
defines = {{ private = ["NOMINMAX"] }}

[targets.time]
type = "static-library"
sources = ["src/time.cc"]

[targets.time.cfg.macos]
link-flags = ["-Wl,-framework,CoreFoundation"]

[targets.vtz]
type = "static-library"
sources = ["src/vtz.cc"]

[targets.vtz.cfg.clang]
cxx-flags = ["-Wshorten-64-to-32"]

[cfg.windows.dependencies.winreg]
git = "https://github.com/example/winreg"
tag = "v1.0"
"#
    );
    let (p, w) = ok(&text);
    assert!(w.0.is_empty(), "no warnings expected: {:?}", w.0);

    let libninja = &p.targets["libninja"];
    assert_eq!(libninja.cfg.len(), 3);
    assert_eq!(libninja.cfg[0].0.atom, CfgAtom::Unix);
    assert_eq!(libninja.cfg[0].1.sources.len(), 2);
    assert_eq!(libninja.cfg[1].0.atom, CfgAtom::Linux);
    assert_eq!(libninja.cfg[1].1.defines.private, vec!["USE_PPOLL"]);
    assert_eq!(libninja.cfg[2].0.atom, CfgAtom::Windows);

    let time = &p.targets["time"];
    // bare list == all-private, for flags exactly like the v0 keys.
    assert_eq!(
        time.cfg[0].1.link_flags.private,
        vec!["-Wl,-framework,CoreFoundation"]
    );

    let winreg = &p.dependencies["winreg"];
    assert_eq!(winreg.cfg.unwrap().atom, CfgAtom::Windows);
    assert!(!winreg.dev);
}

#[test]
fn schema_cfg_groups_preserve_document_order() {
    // Keys chosen so alphabetical order would differ from document order.
    let text = format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.unix]\nsources = [\"u.cpp\"]\n\
         [targets.t.cfg.linux]\nsources = [\"l.cpp\"]\n\
         [targets.t.cfg.clang]\nsources = [\"c.cpp\"]\n"
    );
    let (p, _) = ok(&text);
    let atoms: Vec<CfgAtom> = p.targets["t"].cfg.iter().map(|(pr, _)| pr.atom).collect();
    assert_eq!(atoms, vec![CfgAtom::Unix, CfgAtom::Linux, CfgAtom::Clang]);
}

#[test]
fn schema_cfg_predicate_eval() {
    let mac_clang = CfgTruth { os: CfgAtom::Macos, compiler: CfgAtom::Clang };
    let linux_gcc = CfgTruth { os: CfgAtom::Linux, compiler: CfgAtom::Gcc };
    let win_msvc = CfgTruth { os: CfgAtom::Windows, compiler: CfgAtom::Msvc };
    let pred = |atom| CfgPredicate { atom };
    assert!(pred(CfgAtom::Unix).eval(&mac_clang));
    assert!(pred(CfgAtom::Unix).eval(&linux_gcc));
    assert!(!pred(CfgAtom::Unix).eval(&win_msvc));
    assert!(pred(CfgAtom::Macos).eval(&mac_clang));
    assert!(!pred(CfgAtom::Macos).eval(&linux_gcc));
    assert!(pred(CfgAtom::Clang).eval(&mac_clang));
    assert!(!pred(CfgAtom::Clang).eval(&linux_gcc));
    assert!(pred(CfgAtom::Gcc).eval(&linux_gcc));
    assert!(pred(CfgAtom::Windows).eval(&win_msvc));
    assert!(pred(CfgAtom::Msvc).eval(&win_msvc));
}

#[test]
fn schema_cfg_unknown_atom_lists_vocabulary() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.freebsd]\nsources = [\"f.cpp\"]\n"
    ));
    assert!(e.contains("'freebsd'"), "{e}");
    assert!(e.contains("windows, macos, linux"), "{e}");
    assert!(e.contains("clang, gcc, msvc"), "{e}");
}

#[test]
fn schema_cfg_apple_clang_reserved() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.apple-clang]\ncxx-flags = [\"-Wx\"]\n"
    ));
    assert!(e.contains("apple-clang") && e.contains("reserved"), "{e}");
    assert!(e.contains("matches Apple clang"), "{e}");
}

#[test]
fn schema_cfg_combinators_reserved_quoted_key() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.\"all(linux, gcc)\"]\nsources = [\"x.cpp\"]\n"
    ));
    assert!(e.contains("reserved"), "{e}");
    assert!(e.contains("all(linux, gcc)"), "{e}");
    // The blessed future spelling is named.
    assert!(e.contains("quoted key"), "{e}");
}

#[test]
fn schema_cfg_nested_cfg_is_error() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.linux.cfg.gcc]\nsources = [\"x.cpp\"]\n"
    ));
    assert!(e.contains("nested cfg"), "{e}");
}

#[test]
fn schema_cfg_scalar_override_not_in_v1() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.linux]\ncxx-std = 20\n"
    ));
    assert!(e.contains("conditional scalar overrides are not in v1"), "{e}");
}

#[test]
fn schema_cfg_public_headers_not_conditionable() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.linux.public-headers]\nbase = \".\"\npatterns = [\"*.h\"]\n"
    ));
    assert!(e.contains("total override"), "{e}");
    assert!(e.contains("includes.public"), "the error is the fix: {e}");
}

#[test]
fn schema_cfg_markers_and_run_not_conditionable() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.linux]\ndev = true\n"
    ));
    assert!(e.contains("not cfg-conditional"), "{e}");
}

#[test]
fn schema_cfg_empty_group_warns() {
    let (_, w) = ok(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [targets.t.cfg.windows]\n"
    ));
    assert!(
        w.0.iter().any(|m| m.contains("empty") && m.contains("cfg.windows")),
        "{:?}",
        w.0
    );
}

#[test]
fn schema_cfg_when_rejected_not_reserved() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\nwhen = \"linux\"\n"
    ));
    assert!(e.contains("not part of the language"), "{e}");
    assert!(e.contains("cfg.<predicate>"), "{e}");

    let e = err(&with_dep("d = { git = \"https://x\", tag = \"1\", when = \"linux\" }"));
    assert!(e.contains("not part of the language"), "{e}");
}

#[test]
fn schema_cfg_reserved_package_scope_placements() {
    // [cfg.<pred>.targets.*]
    let e = err(&format!(
        "{MINIMAL}\n[cfg.linux.targets.t]\ntype = \"executable\"\nsources = []\n"
    ));
    assert!(e.contains("reserved"), "{e}");
    assert!(e.contains("targets"), "{e}");

    // [cfg.<pred>.generate.*]
    let e = err(&format!(
        "{MINIMAL}\n[cfg.linux.generate.g]\ncommand = [\"x\"]\nstdout = \"o\"\n"
    ));
    assert!(e.contains("reserved"), "{e}");

    // [cfg.<pred>.flags] is a misplacement, pointed at the real spelling.
    let e = err(&format!("{MINIMAL}\n[cfg.linux.flags]\ncxx-flags = []\n"));
    assert!(e.contains("[flags.cfg.linux]"), "{e}");

    // [profiles.*.cfg.*]
    let e = err(&format!("{MINIMAL}\n[profiles.debug.cfg.linux]\ncxx-flags = []\n"));
    assert!(e.contains("reserved"), "{e}");

    // [target-defaults.cfg.*]
    let e = err(&format!("{MINIMAL}\n[target-defaults.cfg.linux]\ncxx-std = 20\n"));
    assert!(e.contains("reserved"), "{e}");
}

#[test]
fn schema_cfg_dep_declared_twice_is_error() {
    // unconditional + branch
    let e = err(&format!(
        "{MINIMAL}\n[dependencies.zstd]\ngit = \"https://x\"\ntag = \"1\"\n\
         [cfg.linux.dependencies.zstd]\nsystem = true\n"
    ));
    assert!(e.contains("more than once"), "{e}");
    assert!(e.contains("bundled everywhere or system everywhere"), "{e}");

    // two branches
    let e = err(&format!(
        "{MINIMAL}\n[cfg.macos.dependencies.zstd]\ngit = \"https://x\"\ntag = \"1\"\n\
         [cfg.linux.dependencies.zstd]\nsystem = true\n"
    ));
    assert!(e.contains("more than once"), "{e}");
}

// ===========================================================================
// Wave 1 — [flags] + per-target flags (spec §1)
// ===========================================================================

#[test]
fn schema_package_flags_and_cfg_groups() {
    let text = format!(
        "{MINIMAL}\n[flags]\ncxx-flags = [\"-Wall\", \"-Wextra\"]\n\
         [flags.cfg.clang]\ncxx-flags = [\"-Wthread-safety\"]\n\
         [flags.cfg.gcc]\ncxx-flags = [\"-Wno-psabi\"]\n"
    );
    let (p, w) = ok(&text);
    assert_eq!(p.flags.cxx_flags, vec!["-Wall", "-Wextra"]);
    assert_eq!(p.flags.cfg.len(), 2);
    assert_eq!(p.flags.cfg[0].0.atom, CfgAtom::Clang);
    assert_eq!(p.flags.cfg[0].1.cxx_flags, vec!["-Wthread-safety"]);
    assert!(w.0.is_empty(), "{:?}", w.0);
}

#[test]
fn schema_package_flags_no_visibility_split() {
    // [flags] is environment, not interface: the split form must not parse.
    let e = err(&format!(
        "{MINIMAL}\n[flags]\ncxx-flags = {{ public = [\"-Wall\"] }}\n"
    ));
    assert!(!e.is_empty());
}

#[test]
fn schema_target_flags_visibility_split() {
    let text = format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ private = [\"-Werror\"], public = [\"-fno-exceptions\"] }}\n\
         link-flags = [\"-Wl,-framework,CoreFoundation\"]\n"
    );
    let (p, _) = ok(&text);
    let t = &p.targets["lib"];
    assert_eq!(t.cxx_flags.public, vec!["-fno-exceptions"]);
    assert_eq!(t.cxx_flags.private, vec!["-Werror"]);
    assert_eq!(t.link_flags.private, vec!["-Wl,-framework,CoreFoundation"]);
}

#[test]
fn schema_fence_abi_rejected_at_any_target_scope() {
    // Private bucket too — ABI belongs to [flags]/profiles.
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-stdlib=libc++\"]\n"
    ));
    assert!(e.contains("ABI of the entire link closure"), "{e}");
    assert!(e.contains("[flags]") && e.contains("[profiles.*]"), "{e}");

    // Transport spellings never launder ABI payloads through the fence.
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-Wp,-D_GLIBCXX_DEBUG\"]\n"
    ));
    assert!(e.contains("-Wp,-D_GLIBCXX_DEBUG"), "{e}");
    assert!(e.contains("ABI"), "{e}");
}

#[test]
fn schema_fence_warning_class_public_rejected() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-Wall\"] }}\n"
    ));
    assert!(e.contains("warnings are private by nature"), "{e}");
    assert!(e.contains("diagnostic policy"), "{e}");
    // -w too
    let e = err(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-w\"] }}\n"
    ));
    assert!(e.contains("warnings are private"), "{e}");
}

#[test]
fn schema_fence_optdebug_public_rejected() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-O2\"] }}\n"
    ));
    assert!(e.contains("optimization level is the consumer's"), "{e}");
}

#[test]
fn schema_fence_sanitizer_public_error_private_warning() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-fsanitize=address\"] }}\n"
    ));
    assert!(e.contains("sanitizer"), "{e}");

    let (_, w) = ok(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ private = [\"-fsanitize=address\"] }}\n"
    ));
    assert!(
        w.0.iter().any(|m| m.contains("uninstrumented")),
        "{:?}",
        w.0
    );
}

#[test]
fn schema_fence_private_warning_and_opt_flags_allowed() {
    // The whole point of the fence being C, not A: private stays open.
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-Wall\", \"-Wextra\", \"-Werror\", \"-O3\", \"-g\"]\n"
    ));
    assert_eq!(p.targets["t"].cxx_flags.private.len(), 5);
}

#[test]
fn schema_fence_link_flags_public_passes_non_abi() {
    // link-flags public buckets check only ABI/sanitizer; -Wl, transport ok.
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         link-flags = {{ public = [\"-Wl,-framework,CoreFoundation\"] }}\n"
    ));
    assert_eq!(p.targets["lib"].link_flags.public.len(), 1);
}

#[test]
fn schema_fence_public_flags_on_executable_rejected() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.app]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-fno-exceptions\"] }}\n"
    ));
    assert!(e.contains("nothing can consume an executable"), "{e}");
}

#[test]
fn schema_fence_unknown_flags_fail_open() {
    let (p, w) = ok(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = {{ public = [\"-fno-exceptions\", \"-pthread\", \"-mavx2\"] }}\n"
    ));
    assert_eq!(p.targets["lib"].cxx_flags.public.len(), 3);
    assert!(w.0.is_empty(), "{:?}", w.0);
}

#[test]
fn schema_dedicated_key_spellings_warn_not_error() {
    let (p, w) = ok(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-UNDEBUG\", \"-DFOO=1\", \"-Iinc\", \"-std=c++17\"]\n"
    ));
    assert_eq!(p.targets["t"].cxx_flags.private.len(), 4);
    let joined = w.0.join("\n");
    assert!(joined.contains("defines"), "{joined}");
    assert!(joined.contains("includes"), "{joined}");
    assert!(joined.contains("cxx-std"), "{joined}");
}

#[test]
fn schema_fence_applies_inside_cfg_groups() {
    // Non-matching groups are validated too (spec §2.2) — vocabulary and key
    // rules never depend on the current platform.
    let e = err(&format!(
        "{MINIMAL}\n[targets.lib]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         [targets.lib.cfg.windows]\ncxx-flags = {{ public = [\"-Wall\"] }}\n"
    ));
    assert!(e.contains("warnings are private"), "{e}");
}

// ===========================================================================
// Wave 1 — testing surface (spec §3)
// ===========================================================================

#[test]
fn schema_dev_test_markers_and_run_entries() {
    let text = format!(
        r#"{MINIMAL}
[dev-dependencies]
googletest = {{ git = "https://github.com/google/googletest", tag = "v1.17.0", find-package = "GTest" }}

[targets.lib]
type = "static-library"
sources = ["src/a.cpp"]

[targets.testing-lib]
type = "static-library"
dev  = true
sources = ["t/support.cpp"]

[targets.bench]
type = "executable"
dev  = true
sources = ["t/bench.cpp"]

[targets.tests]
type = "executable"
test = true
sources = ["t/tests.cpp"]

[[targets.tests.run]]
name           = "death-bad-env-path"
cwd            = "tzdb-runtime"
env            = {{ VTZ_TZDATA_PATH = "/bad/env/path" }}
env-remove     = ["TZ"]
expect-failure = true

[[targets.tests.run]]
name = "fast"
args = ["--fast"]
"#
    );
    let (p, w) = ok(&text);
    assert!(w.0.is_empty(), "{:?}", w.0);
    assert!(p.dev_dependencies["googletest"].dev);
    assert_eq!(
        p.dev_dependencies["googletest"].find_package.as_deref(),
        Some("GTest")
    );
    assert!(!p.targets["lib"].dev && !p.targets["lib"].test);
    assert!(p.targets["testing-lib"].dev && !p.targets["testing-lib"].test);
    assert!(p.targets["bench"].dev && !p.targets["bench"].test);
    let tests = &p.targets["tests"];
    assert!(tests.test && tests.dev, "test implies dev");
    assert_eq!(tests.run.len(), 2);
    let r0 = &tests.run[0];
    assert_eq!(r0.name.as_deref(), Some("death-bad-env-path"));
    assert_eq!(r0.cwd.as_deref(), Some("tzdb-runtime"));
    assert_eq!(r0.env["VTZ_TZDATA_PATH"], "/bad/env/path");
    assert_eq!(r0.env_remove, vec!["TZ"]);
    assert!(r0.expect_failure);
    assert!(!tests.run[1].expect_failure, "default false");
}

#[test]
fn schema_test_dev_false_reserved() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         test = true\ndev = false\n"
    ));
    assert!(e.contains("reserved"), "{e}");
    assert!(e.contains("test implies dev"), "{e}");
}

#[test]
fn schema_test_on_library_error_with_hint() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\ntest = true\n"
    ));
    assert!(e.contains("only legal on executables"), "{e}");
    assert!(e.contains("`dev = true`"), "{e}");
}

#[test]
fn schema_run_on_non_test_target_error() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         [[targets.t.run]]\nargs = [\"--x\"]\n"
    ));
    assert!(e.contains("only legal on test targets"), "{e}");
}

#[test]
fn schema_run_duplicate_names_and_unknown_fields() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\ntest = true\n\
         [[targets.t.run]]\nname = \"a\"\n[[targets.t.run]]\nname = \"a\"\n"
    ));
    assert!(e.contains("duplicate run entry name 'a'"), "{e}");

    parse_str(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\ntest = true\n\
         [[targets.t.run]]\ntimeout = 5\n"
    ))
    .expect_err("unknown run fields must be rejected");
}

#[test]
fn schema_run_expect_signal_reserved() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\ntest = true\n\
         [[targets.t.run]]\nexpect-signal = \"SIGABRT\"\n"
    ));
    assert!(e.contains("expect-signal") && e.contains("reserved"), "{e}");
    assert!(e.contains("signal death"), "{e}");
}

#[test]
fn schema_dep_dev_dep_key_collision() {
    let e = err(&format!(
        "{MINIMAL}\n[dependencies.gt]\ngit = \"https://x\"\ntag = \"1\"\n\
         [dev-dependencies.gt]\ngit = \"https://x\"\ntag = \"1\"\n"
    ));
    assert!(e.contains("one resolution namespace") || e.contains("share one"), "{e}");
    assert!(e.contains("'gt'"), "{e}");
}

#[test]
fn schema_regular_dep_cannot_need_dev_dep() {
    let e = err(&format!(
        "{MINIMAL}\n[dependencies.a]\ngit = \"https://x\"\ntag = \"1\"\nneeds = [\"gt\"]\n\
         [dev-dependencies.gt]\ngit = \"https://x\"\ntag = \"1\"\n"
    ));
    assert!(e.contains("cannot"), "{e}");
    assert!(e.contains("[dev-dependencies]"), "{e}");
}

#[test]
fn schema_dev_dep_may_need_regular_and_dev() {
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[dependencies.fmt]\ngit = \"https://x\"\ntag = \"1\"\n\
         [dev-dependencies.a]\ngit = \"https://x\"\ntag = \"1\"\nneeds = [\"fmt\", \"b\"]\n\
         [dev-dependencies.b]\ngit = \"https://x\"\ntag = \"1\"\n"
    ));
    assert_eq!(p.dev_dependencies["a"].needs, vec!["fmt", "b"]);
}

// ===========================================================================
// Wave 1 — [generate] (spec §4)
// ===========================================================================

#[test]
fn schema_generate_template_command_checked_in() {
    let text = format!(
        r#"{MINIMAL}
[generate.version-header]
template = "src/version.hpp.in"
output   = "src/version.hpp"
vars     = {{ CMAKE_PROJECT_VERSION = "${{package.version}}" }}

[generate.browse-py-h]
command = ["sh", "src/inline.sh", "kBrowsePy"]
stdin   = "src/browse.py"
stdout  = "build/browse_py.h"
inputs  = ["src/inline.sh"]

[generate.known-zones]
command    = ["python3", "scripts/gen.py", "data/tzdata.zi"]
stdout     = "known_zones.h"
inputs     = ["scripts/gen.py", "data/tzdata.zi"]
checked-in = "include/impl/known_zones.h"
"#
    );
    let (p, _) = ok(&text);
    assert_eq!(p.generate.len(), 3);
    match &p.generate["version-header"].action {
        GenerateAction::Template { template, output, vars } => {
            assert_eq!(template, "src/version.hpp.in");
            assert_eq!(output, "src/version.hpp");
            assert_eq!(vars["CMAKE_PROJECT_VERSION"], "${package.version}");
        }
        other => panic!("wrong action: {other:?}"),
    }
    match &p.generate["browse-py-h"].action {
        GenerateAction::Command { argv, stdin, stdout } => {
            assert_eq!(argv[0], "sh");
            assert_eq!(stdin.as_deref(), Some("src/browse.py"));
            assert_eq!(stdout, "build/browse_py.h");
        }
        other => panic!("wrong action: {other:?}"),
    }
    assert_eq!(
        p.generate["known-zones"].checked_in.as_deref(),
        Some("include/impl/known_zones.h")
    );
    assert!(p.generate["browse-py-h"].checked_in.is_none());
}

#[test]
fn schema_generate_exactly_one_action() {
    let e = err(&format!(
        "{MINIMAL}\n[generate.g]\ntemplate = \"a.in\"\noutput = \"a\"\ncommand = [\"x\"]\nstdout = \"b\"\n"
    ));
    assert!(e.contains("exactly one"), "{e}");
    let e = err(&format!("{MINIMAL}\n[generate.g]\ninputs = [\"x\"]\n"));
    assert!(e.contains("no action"), "{e}");
}

#[test]
fn schema_generate_field_action_mismatch() {
    let e = err(&format!(
        "{MINIMAL}\n[generate.g]\ntemplate = \"a.in\"\noutput = \"a\"\nstdin = \"x\"\n"
    ));
    assert!(e.contains("only apply to"), "{e}");
    let e = err(&format!(
        "{MINIMAL}\n[generate.g]\ncommand = [\"x\"]\nstdout = \"o\"\nvars = {{ A = \"1\" }}\n"
    ));
    assert!(e.contains("only apply to"), "{e}");
    let e = err(&format!("{MINIMAL}\n[generate.g]\ntemplate = \"a.in\"\n"));
    assert!(e.contains("output"), "{e}");
    let e = err(&format!("{MINIMAL}\n[generate.g]\ncommand = [\"x\"]\n"));
    assert!(e.contains("stdout"), "{e}");
    let e = err(&format!("{MINIMAL}\n[generate.g]\ncommand = []\nstdout = \"o\"\n"));
    assert!(e.contains("non-empty argv"), "{e}");
}

#[test]
fn schema_generate_output_path_escapes_refused() {
    for bad in ["/abs/path.h", "../up.h", "a/../../up.h", ""] {
        let e = err(&format!(
            "{MINIMAL}\n[generate.g]\ncommand = [\"x\"]\nstdout = \"{bad}\"\n"
        ));
        assert!(
            e.contains("source-tree writes are refused by construction"),
            "for {bad:?}: {e}"
        );
    }
}

#[test]
fn schema_generate_output_collision_case_insensitive() {
    let e = err(&format!(
        "{MINIMAL}\n[generate.a]\ncommand = [\"x\"]\nstdout = \"Version.h\"\n\
         [generate.b]\ncommand = [\"y\"]\nstdout = \"version.h\"\n"
    ));
    assert!(e.contains("collision"), "{e}");
    assert!(e.contains("case-insensitively"), "{e}");
}

#[test]
fn schema_generate_step_name_charset() {
    let e = err(&format!(
        "{MINIMAL}\n[generate.\"bad name\"]\ncommand = [\"x\"]\nstdout = \"o\"\n"
    ));
    assert!(e.contains("bad name"), "{e}");
}

// ===========================================================================
// Wave 1 — patches, system deps, subdir, builtins (spec §5, A.5)
// ===========================================================================

#[test]
fn schema_patches_parse_in_order() {
    let (p, _) = ok(&with_dep(
        "absl = { git = \"https://x\", rev = \"255c84d\", \
         patches = [\"patches/0001.patch\", \"patches/0002.patch\"] }",
    ));
    let absl = &p.dependencies["absl"];
    assert_eq!(
        absl.patches,
        vec![
            std::path::PathBuf::from("patches/0001.patch"),
            std::path::PathBuf::from("patches/0002.patch")
        ]
    );
}

#[test]
fn schema_patches_duplicates_absolute_and_table_form() {
    let e = err(&with_dep(
        "d = { git = \"https://x\", tag = \"1\", patches = [\"p.patch\", \"p.patch\"] }",
    ));
    assert!(e.contains("duplicate patch"), "{e}");

    let e = err(&with_dep(
        "d = { git = \"https://x\", tag = \"1\", patches = [\"/abs/p.patch\"] }",
    ));
    assert!(e.contains("relative to the manifest"), "{e}");

    let e = err(&with_dep(
        "d = { git = \"https://x\", tag = \"1\", patches = [{ file = \"p.patch\", strip = 2 }] }",
    ));
    assert!(e.contains("reserved"), "{e}");
    assert!(e.contains("strip"), "{e}");
}

#[test]
fn schema_system_dep_parses_with_min_version() {
    let (p, _) = ok(&with_dep("zstd = { system = true, min-version = \"1.5\" }"));
    match &p.dependencies["zstd"].source {
        SourceSpec::System { min_version } => {
            assert_eq!(min_version.as_deref(), Some("1.5"));
        }
        other => panic!("wrong source: {other:?}"),
    }
}

#[test]
fn schema_system_dep_field_exclusions() {
    let e = err(&with_dep("z = { system = true, git = \"https://x\", tag = \"1\" }"));
    assert!(e.contains("mutually exclusive"), "{e}");

    let e = err(&with_dep("z = { system = true, patches = [\"p.patch\"] }"));
    assert!(e.contains("no source tree to patch"), "{e}");

    let e = err(&with_dep("z = { system = true, options = { X = \"ON\" } }"));
    assert!(e.contains("never built"), "{e}");

    let e = err(&with_dep(
        "z = { system = true, needs = [\"w\"] }\nw = { git = \"https://x\", tag = \"1\" }",
    ));
    assert!(e.contains("`needs` on a system dependency"), "{e}");

    let e = err(&with_dep("z = { system = true, subdir = \"build/cmake\" }"));
    assert!(e.contains("subdir"), "{e}");

    let e = err(&with_dep("z = { git = \"https://x\", tag = \"1\", min-version = \"1.5\" }"));
    assert!(e.contains("min-version"), "{e}");
}

#[test]
fn schema_system_dep_pkg_config_reserved() {
    let e = err(&with_dep("z = { system = true, pkg-config = \"libzstd\" }"));
    assert!(e.contains("pkg-config") && e.contains("reserved"), "{e}");
}

#[test]
fn schema_other_deps_may_need_a_system_dep() {
    let (p, _) = ok(&with_dep(
        "z = { system = true }\n\
         a = { git = \"https://x\", tag = \"1\", needs = [\"z\"] }",
    ));
    assert_eq!(p.dependencies["a"].needs, vec!["z"]);
}

#[test]
fn schema_subdir_validation() {
    let (p, _) = ok(&with_dep(
        "zstd = { git = \"https://x\", tag = \"v1.5.7\", subdir = \"build/cmake\" }",
    ));
    assert_eq!(p.dependencies["zstd"].subdir.as_deref(), Some("build/cmake"));

    for bad in ["/abs", "a/../..", ".."] {
        let e = err(&with_dep(&format!(
            "z = {{ git = \"https://x\", tag = \"1\", subdir = \"{bad}\" }}"
        )));
        assert!(e.contains("subdir"), "for {bad:?}: {e}");
    }
}

#[test]
fn schema_dep_system_includes_opt_out() {
    let (p, _) = ok(&with_dep(
        "ftxui = { git = \"https://x\", tag = \"1\", system-includes = false }",
    ));
    assert_eq!(p.dependencies["ftxui"].system_includes, Some(false));
}

#[test]
fn schema_builtin_claims_are_flag_day_errors() {
    // The wave's one deliberate flag-day break: the message IS the fix.
    let e = err(&with_dep(
        "date = { git = \"https://x\", tag = \"1\", exposes-targets = [\"Threads::Threads\"] }",
    ));
    assert!(e.contains("builtin pseudo-package; delete this line"), "{e}");

    let e = err(&with_dep(
        "date = { git = \"https://x\", tag = \"1\", exposes-targets = { \"Threads::Threads\" = \"t\" } }",
    ));
    assert!(e.contains("builtin pseudo-package; delete this line"), "{e}");

    let e = err(&with_dep(
        "date = { git = \"https://x\", tag = \"1\", exposes-namespace = [\"Threads\"] }",
    ));
    assert!(e.contains("builtin pseudo-package; delete this line"), "{e}");
}

#[test]
fn schema_threads_reference_needs_no_declaration() {
    // Referencing the builtin is plain data at schema level (the ladder's
    // step 0 lives in graph); the manifest parses with zero declarations.
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[targets.bench]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         dependencies = {{ private = [\"Threads::Threads\"] }}\n"
    ));
    assert_eq!(
        p.targets["bench"].dependencies.private,
        vec!["Threads::Threads"]
    );
    assert!(cppkg::schema::BUILTIN_TARGETS.contains(&"Threads::Threads"));
}

// ===========================================================================
// Wave 1 — install/export surface (spec §6)
// ===========================================================================

#[test]
fn schema_export_defaults_and_override() {
    let (p, _) = ok(MINIMAL);
    assert_eq!(p.export.cmake_name, "app");
    assert_eq!(p.export.namespace, "app");

    let (p, _) = ok(&format!(
        "{MINIMAL}\n[export]\ncmake-name = \"GTest\"\nnamespace = \"GTest\"\n"
    ));
    assert_eq!(p.export.cmake_name, "GTest");
    assert_eq!(p.export.namespace, "GTest");

    let e = err(&format!("{MINIMAL}\n[export]\nnamespace = \"a::b\"\n"));
    assert!(e.contains("a::b"), "{e}");
}

#[test]
fn schema_install_and_public_headers_surface() {
    let text = format!(
        r#"{MINIMAL}
[targets.vtz]
type = "static-library"
sources = ["src/a.cpp"]
includes = {{ public = ["include/api"], private = ["include/impl"] }}
install = true

[targets.absl-like]
type = "static-library"
sources = ["src/b.cpp"]
install = true
public-headers = {{ base = ".", patterns = ["absl/**/*.h", "absl/**/*.inc"] }}
"#
    );
    let (p, _) = ok(&text);
    assert!(p.targets["vtz"].install);
    assert!(p.targets["vtz"].public_headers.is_none(), "derived by default");
    let ph = p.targets["absl-like"].public_headers.as_ref().unwrap();
    assert_eq!(ph.base, ".");
    assert_eq!(ph.patterns.len(), 2);
}

#[test]
fn schema_install_on_dev_or_test_target_error() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         test = true\ninstall = true\n"
    ));
    assert!(e.contains("test targets are excluded from export"), "{e}");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         dev = true\ninstall = true\n"
    ));
    assert!(e.contains("dev targets are excluded from export"), "{e}");
}

#[test]
fn schema_runtime_data_defaults_and_validation() {
    let text = format!(
        r#"{MINIMAL}
[targets.cppcheck]
type = "executable"
sources = ["a.cpp"]
runtime-data = [
  {{ from = "cfg", patterns = ["*.cfg"] }},
  {{ from = "platforms", patterns = ["*.xml", "!*-unsigned.xml"] }},
  {{ from = "addons" }},
  {{ from = "data/tzdata/" }},
]
"#
    );
    let (p, _) = ok(&text);
    let rd = &p.targets["cppcheck"].runtime_data;
    assert_eq!(rd[0].to, "cfg");
    assert_eq!(rd[1].patterns, vec!["*.xml", "!*-unsigned.xml"]);
    assert_eq!(rd[2].patterns, vec!["**/*"], "default pattern");
    assert_eq!(rd[2].to, "addons", "default to = last component");
    assert_eq!(rd[3].from, "data/tzdata", "trailing slash trimmed");
    assert_eq!(rd[3].to, "tzdata");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         runtime-data = [{{ from = \"cfg\", patterns = [\"!*.bad\"] }}]\n"
    ));
    assert!(e.contains("only '!' negations"), "{e}");
}

// ===========================================================================
// Wave 1 — glob negation surface (spec §7.1 / §0.4)
// ===========================================================================

#[test]
fn schema_sources_negative_patterns() {
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[targets.cli-lib]\ntype = \"static-library\"\n\
         sources = [\"cli/*.cpp\", \"!cli/main.cpp\"]\n"
    ));
    assert_eq!(p.targets["cli-lib"].sources[1], "!cli/main.cpp");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"!a.cpp\", \"!b.cpp\"]\n"
    ));
    assert!(e.contains("at least one positive pattern"), "{e}");
}

#[test]
fn schema_cfg_group_may_be_all_negative() {
    // A cfg group refines a non-empty base list; appending a negation is the
    // sanctioned way to subtract per-platform.
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"src/*.cpp\"]\n\
         [targets.t.cfg.windows]\nsources = [\"!src/posix.cpp\"]\n"
    ));
    assert_eq!(p.targets["t"].cfg[0].1.sources, vec!["!src/posix.cpp"]);
}

// ===========================================================================
// Wave 1 — [target-defaults] (spec §7.2)
// ===========================================================================

#[test]
fn schema_target_defaults_merge_and_eligibility() {
    let text = format!(
        r#"{MINIMAL}
[target-defaults]
cxx-std = 17
defines = {{ private = ["HAVE_RULES=0"] }}
install = true

[targets.lib]
type = "static-library"
sources = ["a.cpp"]
defines = {{ private = ["OWN"] }}

[targets.newer]
type = "static-library"
sources = ["b.cpp"]
cxx-std = 20

[targets.opted-out]
type = "executable"
sources = ["c.cpp"]
install = false

[targets.tests]
type = "executable"
test = true
sources = ["t.cpp"]

[targets.bench]
type = "executable"
dev = true
sources = ["d.cpp"]
"#
    );
    let (p, _) = ok(&text);
    // Scalars fill-if-absent; the target's own value wins.
    assert_eq!(p.targets["lib"].cxx_std, Some(17));
    assert_eq!(p.targets["newer"].cxx_std, Some(20));
    // Lists prepend: defaults first, target entries after.
    assert_eq!(p.targets["lib"].defines.private, vec!["HAVE_RULES=0", "OWN"]);
    // install fills eligible targets only: dev/test are skipped, an explicit
    // false wins over the default.
    assert!(p.targets["lib"].install);
    assert!(!p.targets["opted-out"].install);
    assert!(!p.targets["tests"].install);
    assert!(!p.targets["bench"].install);
    // Raw table retained for --query.
    assert!(p.target_defaults_raw.is_some());
}

#[test]
fn schema_target_defaults_public_headers_only_onto_installing_libraries() {
    let text = format!(
        r#"{MINIMAL}
[target-defaults]
install = true
public-headers = {{ base = ".", patterns = ["absl/**/*.h"] }}

[targets.lib]
type = "static-library"
sources = ["a.cpp"]

[targets.app]
type = "executable"
sources = ["m.cpp"]

[targets.testonly]
type = "static-library"
dev = true
sources = ["t.cpp"]
"#
    );
    let (p, _) = ok(&text);
    assert!(p.targets["lib"].install && p.targets["lib"].public_headers.is_some());
    assert!(p.targets["app"].install, "executables do install");
    assert!(p.targets["app"].public_headers.is_none(), "but ship no headers");
    assert!(!p.targets["testonly"].install);
    assert!(p.targets["testonly"].public_headers.is_none());
}

#[test]
fn schema_target_defaults_runtime_data_fills_everywhere() {
    let text = format!(
        r#"{MINIMAL}
[target-defaults]
runtime-data = [{{ from = "cfg" }}]

[targets.tests]
type = "executable"
test = true
sources = ["t.cpp"]
"#
    );
    let (p, _) = ok(&text);
    assert_eq!(p.targets["tests"].runtime_data[0].from, "cfg");
}

#[test]
fn schema_target_defaults_rejected_keys() {
    let e = err(&format!("{MINIMAL}\n[target-defaults]\ncxx-flags = [\"-Wall\"]\n"));
    assert!(e.contains("[flags]"), "reserved error must point at [flags]: {e}");

    let e = err(&format!("{MINIMAL}\n[target-defaults]\ndependencies = [\"fmt::fmt\"]\n"));
    assert!(e.contains("unreadable"), "{e}");

    let e = err(&format!("{MINIMAL}\n[target-defaults]\ndev = true\n"));
    assert!(e.contains("reclassifies graph membership"), "{e}");

    let e = err(&format!("{MINIMAL}\n[target-defaults]\ntype = \"executable\"\n"));
    assert!(e.contains("cannot be defaulted"), "{e}");

    let e = err(&format!("{MINIMAL}\n[target-defaults]\nfrobnicate = 1\n"));
    assert!(e.contains("unknown key"), "{e}");
}

// ===========================================================================
// Wave 1 — interpolation placement (spec §0.3)
// ===========================================================================

#[test]
fn schema_interp_whitelisted_positions_accepted() {
    let text = format!(
        r#"{MINIMAL}
[generate.zic]
command = ["zic", "-d", "${{gen}}/zoneinfo", "data/tzdata.zi"]
stdout  = "zic.log"

[targets.t]
type = "executable"
sources = ["src/main.cpp", "${{gen}}/src/version.cpp"]
includes = {{ private = ["${{gen}}/src"] }}
defines = {{ private = ['VERSION="v${{package.version}}"', 'FILESDIR="${{install-prefix}}/share/x"'] }}
test = true

[[targets.t.run]]
args = ["--data", "${{gen}}/zoneinfo"]
cwd  = "build/scratch"
env  = {{ VTZ_TZDATA_PATH = "${{gen}}/zoneinfo", ROOT = "${{project-root}}" }}
"#
    );
    ok(&text);
}

#[test]
fn schema_interp_forbidden_positions_rejected() {
    // Flag lists are not an interpolation position.
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-DX=${{package.version}}\"]\n"
    ));
    assert!(e.contains("not available in this position"), "{e}");

    // Dependency options are not either.
    let e = err(&with_dep(
        "d = { git = \"https://x\", tag = \"1\", options = { V = \"${package.version}\" } }",
    ));
    assert!(e.contains("not available in this position"), "{e}");

    // The key part of a define may not interpolate.
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         defines = [\"${{package.name}}_X=1\"]\n"
    ));
    assert!(e.contains("value part"), "{e}");
}

#[test]
fn schema_interp_public_headers_base_names_the_route() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\n\
         public-headers = {{ base = \"${{gen}}\", patterns = [\"*.h\"] }}\n"
    ));
    assert!(e.contains("includes.public"), "should name the sanctioned route: {e}");
}

#[test]
fn schema_interp_escape_allowed_anywhere() {
    // `$${` is a literal `${` and passes placement checks even in forbidden
    // positions.
    ok(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\n\
         cxx-flags = [\"-DRAW=$${{not_interp}}\"]\n"
    ));
}

// ===========================================================================
// Wave 1 — misc: DependencySpec helper, v0 byte-compat spot checks
// ===========================================================================

#[test]
fn schema_dependency_from_source_helper() {
    let spec = DependencySpec::from_source(SourceSpec::Git {
        url: "https://x".into(),
        reference: GitRef::Tag("1".into()),
    });
    assert!(spec.patches.is_empty() && spec.subdir.is_none());
    assert!(spec.cfg.is_none() && !spec.dev);
}

#[test]
fn schema_v0_manifest_unchanged_defaults() {
    // An unmarked v0 manifest gets pure defaults on every wave-1 field —
    // byte-identical behavior downstream (spec §3.2).
    let (p, w) = ok(&format!(
        "{MINIMAL}\n[dependencies.fmt]\ngit = \"https://x\"\ntag = \"1\"\n\
         [targets.app]\ntype = \"executable\"\nsources = [\"m.cpp\"]\n"
    ));
    assert!(w.0.is_empty());
    assert!(p.flags.cxx_flags.is_empty() && p.flags.cfg.is_empty());
    assert!(p.dev_dependencies.is_empty() && p.generate.is_empty());
    assert!(p.target_defaults_raw.is_none());
    let t = &p.targets["app"];
    assert!(!t.dev && !t.test && !t.install);
    assert!(t.run.is_empty() && t.cfg.is_empty() && t.runtime_data.is_empty());
    assert!(t.public_headers.is_none() && t.system_includes.is_none());
    let d = &p.dependencies["fmt"];
    assert!(d.patches.is_empty() && d.subdir.is_none() && d.cfg.is_none() && !d.dev);
}

#[test]
fn schema_reserved_target_spellings_have_distinct_errors() {
    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\nexceptions = false\n"
    ));
    assert!(e.contains("reserved") && e.contains("-fno-exceptions"), "{e}");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"static-library\"\nsources = [\"a.cpp\"]\nrtti = false\n"
    ));
    assert!(e.contains("reserved"), "{e}");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\nframeworks = [\"CoreFoundation\"]\n"
    ));
    assert!(e.contains("reserved") && e.contains("link-flags"), "{e}");

    let e = err(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = [\"a.cpp\"]\ncxx-extensions = true\n"
    ));
    assert!(e.contains("reserved"), "{e}");
}

#[test]
fn schema_cfg_scope_dev_dependencies_and_empty_scope_warning() {
    let (p, _) = ok(&format!(
        "{MINIMAL}\n[cfg.linux.dev-dependencies.perf-helper]\ngit = \"https://x\"\ntag = \"1\"\n"
    ));
    let d = &p.dev_dependencies["perf-helper"];
    assert!(d.dev);
    assert_eq!(d.cfg.unwrap().atom, CfgAtom::Linux);

    let (_, w) = ok(&format!("{MINIMAL}\n[cfg.linux]\n"));
    assert!(
        w.0.iter().any(|m| m.contains("[cfg.linux]") && m.contains("empty")),
        "{:?}",
        w.0
    );
}

#[test]
fn schema_sources_still_required() {
    // v0 strictness preserved: a target without `sources` is a parse error,
    // not a silently empty target.
    let e = err(&format!("{MINIMAL}\n[targets.t]\ntype = \"executable\"\n"));
    assert!(e.contains("sources"), "{e}");
}
