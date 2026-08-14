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
    // Target table.
    parse_str(&format!(
        "{MINIMAL}\n[targets.t]\ntype = \"executable\"\nsources = []\nlink-flags = []\n"
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
