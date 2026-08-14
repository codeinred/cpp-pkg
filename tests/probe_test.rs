//! End-to-end probe tests against real cmake/ninja: script a small installed
//! package pair into a tempdir, probe it, and check the extracted records.
//!
//! Fixture shape (all local, no network):
//!   depa — plain installed STATIC lib, namespace DepA::
//!   fix  — STATIC lib `fixcore` with a PRIVATE dep on DepA::depa (the
//!          export turns that into $<LINK_ONLY:DepA::depa>) plus an
//!          INTERFACE lib `fixheader`; namespace Fix::; fixConfig.cmake does
//!          find_dependency(depa) then includes the targets file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cppkg::probe::{probe_installed, split_cmake_list, ProbeRecord, RAW_LINK_LIBRARIES_PROP};
use cppkg::schema::BuildConfig;
use cppkg::toolchain::{Dialect, Toolchain, ToolchainIdentity};

/// A hand-built toolchain (toolchain::detect is exercised in its own module;
/// the probe only consumes the struct fields).
fn test_toolchain() -> Toolchain {
    Toolchain {
        cxx: PathBuf::from("/usr/bin/c++"),
        cc: PathBuf::from("/usr/bin/cc"),
        ar: PathBuf::from("/usr/bin/ar"),
        sdk_path: None,
        identity: ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id: "AppleClang".to_string(),
            version: "0".to_string(),
            target_triple: "arm64-apple-darwin".to_string(),
            stdlib: "libc++".to_string(),
            stdlib_version: "0".to_string(),
            sdk_version: None,
        },
    }
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn run_cmake(args: &[&str]) {
    let out = Command::new("cmake").args(args).output().expect("run cmake");
    assert!(
        out.status.success(),
        "cmake {:?} failed:\n{}\n{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn install_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let depa_prefix = root.join("prefix-depa");
    let fix_prefix = root.join("prefix-fix");

    let depa = root.join("src-depa");
    write(
        &depa.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.24)
project(depa LANGUAGES CXX)
add_library(depa STATIC src/depa.cpp)
target_include_directories(depa PUBLIC $<INSTALL_INTERFACE:include>)
install(TARGETS depa EXPORT depaTargets ARCHIVE DESTINATION lib)
install(DIRECTORY include/ DESTINATION include)
install(EXPORT depaTargets NAMESPACE DepA:: FILE depaTargets.cmake DESTINATION lib/cmake/depa)
install(FILES cmake/depaConfig.cmake DESTINATION lib/cmake/depa)
"#,
    );
    write(
        &depa.join("cmake/depaConfig.cmake"),
        "include(\"${CMAKE_CURRENT_LIST_DIR}/depaTargets.cmake\")\n",
    );
    write(&depa.join("include/depa.hpp"), "int depa_value();\n");
    write(&depa.join("src/depa.cpp"), "int depa_value() { return 7; }\n");

    let fix = root.join("src-fix");
    write(
        &fix.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.24)
project(fix LANGUAGES CXX)
find_package(depa REQUIRED CONFIG)
add_library(fixcore STATIC src/fixcore.cpp)
target_include_directories(fixcore INTERFACE $<INSTALL_INTERFACE:include>)
target_compile_definitions(fixcore INTERFACE FIXCORE_ENABLED=1)
target_link_libraries(fixcore PRIVATE DepA::depa)
add_library(fixheader INTERFACE)
target_include_directories(fixheader INTERFACE $<INSTALL_INTERFACE:include>)
target_compile_definitions(fixheader INTERFACE FIXHEADER_ONLY=1)
install(TARGETS fixcore fixheader EXPORT fixTargets ARCHIVE DESTINATION lib)
install(DIRECTORY include/ DESTINATION include)
install(EXPORT fixTargets NAMESPACE Fix:: FILE fixTargets.cmake DESTINATION lib/cmake/fix)
install(FILES cmake/fixConfig.cmake DESTINATION lib/cmake/fix)
"#,
    );
    write(
        &fix.join("cmake/fixConfig.cmake"),
        "include(CMakeFindDependencyMacro)\n\
         find_dependency(depa)\n\
         include(\"${CMAKE_CURRENT_LIST_DIR}/fixTargets.cmake\")\n",
    );
    write(&fix.join("include/fix.hpp"), "int fix_value();\n");
    write(
        &fix.join("src/fixcore.cpp"),
        "#include \"depa.hpp\"\nint fix_value() { return depa_value() + 1; }\n",
    );

    let depa_build = root.join("build-depa");
    run_cmake(&[
        "-G",
        "Ninja",
        "-S",
        depa.to_str().unwrap(),
        "-B",
        depa_build.to_str().unwrap(),
        "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_CXX_COMPILER=/usr/bin/c++",
        &format!("-DCMAKE_INSTALL_PREFIX={}", depa_prefix.display()),
    ]);
    run_cmake(&["--build", depa_build.to_str().unwrap()]);
    run_cmake(&["--install", depa_build.to_str().unwrap()]);

    let fix_build = root.join("build-fix");
    run_cmake(&[
        "-G",
        "Ninja",
        "-S",
        fix.to_str().unwrap(),
        "-B",
        fix_build.to_str().unwrap(),
        "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_CXX_COMPILER=/usr/bin/c++",
        &format!("-DCMAKE_INSTALL_PREFIX={}", fix_prefix.display()),
        &format!("-DCMAKE_PREFIX_PATH={}", depa_prefix.display()),
    ]);
    run_cmake(&["--build", fix_build.to_str().unwrap()]);
    run_cmake(&["--install", fix_build.to_str().unwrap()]);

    (fix_prefix, depa_prefix)
}

fn value_of<'a>(records: &'a [ProbeRecord], target: &str, prop: &str) -> Option<&'a str> {
    records
        .iter()
        .find(|r| r.target == target && r.property == prop)
        .map(|r| r.value.as_str())
}

#[test]
fn probe_installed_extracts_fixture_package() {
    let tmp = tempfile::tempdir().unwrap();
    let (fix_prefix, depa_prefix) = install_fixture(tmp.path());

    let records = probe_installed(
        "fix",
        &[fix_prefix.clone(), depa_prefix.clone()],
        BuildConfig::Release,
        &test_toolchain(),
        &tmp.path().join("probe-work"),
    )
    .unwrap();

    // Both fix targets discovered; the transitive find_dependency target
    // appears in the diff too (attribution happens later, by namespace).
    let targets: std::collections::BTreeSet<&str> =
        records.iter().map(|r| r.target.as_str()).collect();
    assert!(targets.contains("Fix::fixcore"), "targets: {targets:?}");
    assert!(targets.contains("Fix::fixheader"), "targets: {targets:?}");
    assert!(targets.contains("DepA::depa"), "targets: {targets:?}");

    assert_eq!(
        value_of(&records, "Fix::fixcore", "TYPE"),
        Some("STATIC_LIBRARY")
    );
    assert_eq!(
        value_of(&records, "Fix::fixheader", "TYPE"),
        Some("INTERFACE_LIBRARY")
    );

    // Location: active-config record present and pointing at the archive;
    // fallback-rule inputs present as records (values may be empty).
    let loc = value_of(&records, "Fix::fixcore", "IMPORTED_LOCATION_RELEASE").unwrap();
    assert!(loc.ends_with("libfixcore.a"), "location: {loc}");
    assert_eq!(
        value_of(&records, "Fix::fixcore", "IMPORTED_CONFIGURATIONS"),
        Some("RELEASE")
    );
    assert!(value_of(&records, "Fix::fixcore", "IMPORTED_LOCATION").is_some());
    assert!(value_of(&records, "Fix::fixcore", "MAP_IMPORTED_CONFIG_RELEASE").is_some());

    // Interface include dirs resolve to the install prefix. Canonicalize
    // both sides: macOS temp paths mix /var and /private/var spellings.
    let includes_raw = value_of(&records, "Fix::fixcore", "INTERFACE_INCLUDE_DIRECTORIES").unwrap();
    let includes: Vec<PathBuf> = split_cmake_list(includes_raw)
        .iter()
        .map(|p| fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)))
        .collect();
    let expected = fs::canonicalize(fix_prefix.join("include")).unwrap();
    assert!(
        includes.contains(&expected),
        "includes {includes:?} missing {expected:?}"
    );

    let defines = split_cmake_list(
        value_of(&records, "Fix::fixcore", "INTERFACE_COMPILE_DEFINITIONS").unwrap(),
    );
    assert!(
        defines.contains(&"FIXCORE_ENABLED=1".to_string()),
        "{defines:?}"
    );
    let hdr_defines = split_cmake_list(
        value_of(&records, "Fix::fixheader", "INTERFACE_COMPILE_DEFINITIONS").unwrap(),
    );
    assert!(
        hdr_defines.contains(&"FIXHEADER_ONLY=1".to_string()),
        "{hdr_defines:?}"
    );

    // The PRIVATE static-lib dependency exports as $<LINK_ONLY:...>: intact
    // in the raw record; collapsed to its bare content (still a link entry,
    // marker gone) in the evaluated record. Verified CMake 4.4 behavior:
    // TARGET_PROPERTY leaves INTERFACE_LINK_LIBRARIES unevaluated, so the
    // probe wraps it in TARGET_GENEX_EVAL — see src/probe.rs.
    let raw = value_of(&records, "Fix::fixcore", RAW_LINK_LIBRARIES_PROP).unwrap();
    assert!(
        raw.contains("$<LINK_ONLY:DepA::depa>"),
        "raw link libraries: {raw:?}"
    );
    let evaluated = value_of(&records, "Fix::fixcore", "INTERFACE_LINK_LIBRARIES").unwrap();
    assert!(
        !evaluated.contains("$<LINK_ONLY"),
        "LINK_ONLY marker must be flattened in evaluated value: {evaluated:?}"
    );
    assert!(
        split_cmake_list(evaluated).contains(&"DepA::depa".to_string()),
        "evaluated link libraries should keep the flattened entry: {evaluated:?}"
    );

    // The transitive target's own usage requirements were captured too.
    let depa_includes = value_of(&records, "DepA::depa", "INTERFACE_INCLUDE_DIRECTORIES").unwrap();
    assert!(depa_includes.contains("include"), "{depa_includes:?}");
    let depa_loc = value_of(&records, "DepA::depa", "IMPORTED_LOCATION_RELEASE").unwrap();
    assert!(depa_loc.ends_with("libdepa.a"), "{depa_loc:?}");
}

/// Probing in Debug against a Release-only install: the active-config
/// location is empty, but the fallback inputs (IMPORTED_CONFIGURATIONS +
/// the other per-config locations) let manifest.rs apply CMake's rules.
#[test]
fn probe_installed_debug_config_gets_fallback_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let (fix_prefix, depa_prefix) = install_fixture(tmp.path());

    let records = probe_installed(
        "fix",
        &[fix_prefix, depa_prefix],
        BuildConfig::Debug,
        &test_toolchain(),
        &tmp.path().join("probe-work"),
    )
    .unwrap();

    // Missing property still yields a record, with an empty value.
    assert_eq!(
        value_of(&records, "Fix::fixcore", "IMPORTED_LOCATION_DEBUG"),
        Some("")
    );
    assert_eq!(
        value_of(&records, "Fix::fixcore", "IMPORTED_CONFIGURATIONS"),
        Some("RELEASE")
    );
    let release_loc = value_of(&records, "Fix::fixcore", "IMPORTED_LOCATION_RELEASE").unwrap();
    assert!(release_loc.ends_with("libfixcore.a"), "{release_loc:?}");
    assert!(value_of(&records, "Fix::fixcore", "MAP_IMPORTED_CONFIG_DEBUG").is_some());
}

#[test]
fn probe_installed_missing_package_reports_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let err = probe_installed(
        "definitely-not-installed-cppkg",
        &[tmp.path().join("empty-prefix")],
        BuildConfig::Release,
        &test_toolchain(),
        &tmp.path().join("probe-work"),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("definitely-not-installed-cppkg"),
        "error should name the package: {msg}"
    );
    assert!(
        msg.contains("CMake logs"),
        "error should point at logs: {msg}"
    );
}

#[test]
fn probe_installed_config_not_found_gets_find_package_hint() {
    // A.4: the probe's raw config-not-found error translates into the
    // find-package hint, listing the config names actually installed in the
    // probed prefixes (the googletest -> GTest shape).
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join("prefix");
    write(
        &prefix.join("lib/cmake/GTest/GTestConfig.cmake"),
        "# placeholder — never parsed, the probe fails before reading it\n",
    );
    let err = probe_installed(
        "googletest",
        &[prefix],
        BuildConfig::Release,
        &test_toolchain(),
        &tmp.path().join("probe-work"),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("find-package = \"<Name>\""),
        "should carry the find-package hint: {msg}"
    );
    assert!(
        msg.contains("GTest"),
        "should list the installed config names: {msg}"
    );
    assert!(msg.contains("CMake logs"), "{msg}");
}

#[test]
fn probe_system_not_found_offers_both_worlds() {
    // §5.3: an uninstalled system dependency errors with both fixes —
    // declare it fetched, or install it.
    let tmp = tempfile::tempdir().unwrap();
    let err = cppkg::probe::probe_system(
        "definitely-not-a-system-pkg",
        "definitely-not-a-system-pkg",
        None,
        &test_toolchain(),
        &tmp.path().join("sysdep-work"),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("found nothing on this machine"), "{msg}");
    assert!(msg.contains("fetched dependency (git/url)"), "{msg}");
    assert!(
        msg.contains("brew install definitely-not-a-system-pkg"),
        "{msg}"
    );
}

#[test]
fn probe_system_extracts_zlib_when_present() {
    // Smoke for the "cmake" resolution mode against a real machine package.
    // ZLIB ships with every macOS SDK and every Arch install; if this
    // machine genuinely has none, skip rather than fake a result.
    let tmp = tempfile::tempdir().unwrap();
    let result = cppkg::probe::probe_system(
        "zlib",
        "ZLIB",
        Some("1.0"),
        &test_toolchain(),
        &tmp.path().join("sysdep-work"),
    );
    let (manifest, facts) = match result {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("found nothing on this machine") {
                eprintln!("skipping: no system zlib on this machine");
                return;
            }
            panic!("probe_system(ZLIB) failed: {msg}");
        }
    };
    let comp = manifest
        .components
        .get("ZLIB::ZLIB")
        .expect("FindZLIB should import ZLIB::ZLIB");
    // The single machine artifact is replicated across configs so any
    // consumer config links the file the sysdep hash describes.
    assert_eq!(comp.location.get("Release"), comp.location.get("Debug"));
    assert!(
        !facts.library_paths.is_empty(),
        "resolved libraries should be recorded"
    );
    assert_eq!(facts.library_paths.len(), facts.library_hashes.len());
    assert!(
        facts.library_hashes.iter().all(|h| h.starts_with("blake3:")),
        "{:?}",
        facts.library_hashes
    );
    let mut sorted = facts.library_paths.clone();
    sorted.sort();
    assert_eq!(facts.library_paths, sorted, "paths must be sorted");
    assert!(
        !facts.resolved_version.is_empty(),
        "FindZLIB reports ZLIB_VERSION"
    );
}
