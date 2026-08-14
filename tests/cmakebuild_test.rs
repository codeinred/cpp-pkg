//! Integration tests for cmake_build: real cmake + ninja + the host
//! toolchain against tiny fixture projects written into tempdirs. These live
//! outside the crate (public API only) so they compile independently of any
//! in-crate #[cfg(test)] code.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cppkg::cmake_build::{build_dependency, scrubbed_env, write_toolchain_file, DepBuildRequest};
use cppkg::schema::{BuildConfig, DependencySpec, ExposesTargets, GitRef, SourceSpec};
use cppkg::toolchain::{Dialect, Toolchain, ToolchainIdentity};

/// Prefer the real detector; while it is still unimplemented (todo!()), fall
/// back to a hand-constructed toolchain for this macOS host.
fn test_toolchain() -> Toolchain {
    let hook = std::panic::take_hook();
    // Silence the todo!() panic backtrace while probing the detector.
    std::panic::set_hook(Box::new(|_| {}));
    let detected = std::panic::catch_unwind(cppkg::toolchain::detect_default);
    std::panic::set_hook(hook);
    if let Ok(Ok(tc)) = detected {
        return tc;
    }
    let sdk_path = Command::new("xcrun")
        .arg("--show-sdk-path")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .filter(|p| p.exists());
    Toolchain {
        cxx: PathBuf::from("/usr/bin/c++"),
        cc: PathBuf::from("/usr/bin/cc"),
        ar: PathBuf::from("/usr/bin/ar"),
        sdk_path,
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

fn dummy_spec(options: BTreeMap<String, String>) -> DependencySpec {
    DependencySpec {
        source: SourceSpec::Git {
            url: "https://example.invalid/hello.git".to_string(),
            reference: GitRef::Tag("v1.0.0".to_string()),
        },
        options,
        needs: vec![],
        find_package: None,
        exposes_namespace: vec![],
        exposes_targets: ExposesTargets::default(),
    }
}

fn write_file(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A minimal installable CMake package: static lib + install(EXPORT) +
/// hand-written Config/ConfigVersion files (version 1.0.0).
fn write_hello_fixture(dir: &Path) {
    write_file(
        &dir.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.15)
project(hello VERSION 1.0.0 LANGUAGES CXX)
add_library(hello STATIC src/hello.cpp)
target_include_directories(hello PUBLIC
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
  $<INSTALL_INTERFACE:include>)
install(TARGETS hello EXPORT helloTargets ARCHIVE DESTINATION lib)
install(DIRECTORY include/ DESTINATION include)
install(EXPORT helloTargets NAMESPACE hello:: DESTINATION lib/cmake/hello)
install(FILES cmake/helloConfig.cmake cmake/helloConfigVersion.cmake
        DESTINATION lib/cmake/hello)
"#,
    );
    write_file(
        &dir.join("include/hello.h"),
        "#pragma once\nint hello_add(int a, int b);\n",
    );
    write_file(
        &dir.join("src/hello.cpp"),
        "#include \"hello.h\"\nint hello_add(int a, int b) { return a + b; }\n",
    );
    write_file(
        &dir.join("cmake/helloConfig.cmake"),
        "include(\"${CMAKE_CURRENT_LIST_DIR}/helloTargets.cmake\")\n",
    );
    write_file(
        &dir.join("cmake/helloConfigVersion.cmake"),
        r#"set(PACKAGE_VERSION "1.0.0")
if(PACKAGE_VERSION VERSION_LESS PACKAGE_FIND_VERSION)
  set(PACKAGE_VERSION_COMPATIBLE FALSE)
else()
  set(PACKAGE_VERSION_COMPATIBLE TRUE)
endif()
"#,
    );
}

#[test]
fn cmakebuild_toolchain_file_deterministic_content() {
    let tmp = tempfile::tempdir().unwrap();
    let tc = Toolchain {
        cxx: PathBuf::from("/usr/bin/c++"),
        cc: PathBuf::from("/usr/bin/cc"),
        ar: PathBuf::from("/usr/bin/ar"),
        sdk_path: Some(PathBuf::from("/fake/SDKs/MacOSX.sdk")),
        identity: ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id: "AppleClang".to_string(),
            version: "17.0.0".to_string(),
            target_triple: "arm64-apple-darwin".to_string(),
            stdlib: "libc++".to_string(),
            stdlib_version: "190000".to_string(),
            sdk_version: Some("15.0".to_string()),
        },
    };
    let flags = vec![
        "-D_GLIBCXX_ASSERTIONS".to_string(),
        "-stdlib=libc++".to_string(),
    ];

    let p1 = write_toolchain_file(&tmp.path().join("a"), &tc, &flags).unwrap();
    let p2 = write_toolchain_file(&tmp.path().join("b"), &tc, &flags).unwrap();
    let c1 = fs::read_to_string(&p1).unwrap();
    let c2 = fs::read_to_string(&p2).unwrap();
    assert_eq!(c1, c2, "identical inputs must produce identical bytes");

    assert!(c1.contains("set(CMAKE_C_COMPILER \"/usr/bin/cc\")"));
    assert!(c1.contains("set(CMAKE_CXX_COMPILER \"/usr/bin/c++\")"));
    assert!(c1.contains("set(CMAKE_AR \"/usr/bin/ar\")"));
    assert!(c1.contains("set(CMAKE_OSX_SYSROOT \"/fake/SDKs/MacOSX.sdk\")"));
    // Flags route by language: -stdlib=* reaches only the C++ driver.
    assert!(c1.contains("set(CMAKE_C_FLAGS_INIT \"-D_GLIBCXX_ASSERTIONS\")"));
    assert!(c1.contains("set(CMAKE_CXX_FLAGS_INIT \"-D_GLIBCXX_ASSERTIONS -stdlib=libc++\")"));

    // No SDK, no flags: the corresponding lines must be absent entirely.
    let tc_bare = Toolchain {
        sdk_path: None,
        ..tc
    };
    let p3 = write_toolchain_file(&tmp.path().join("c"), &tc_bare, &[]).unwrap();
    let c3 = fs::read_to_string(&p3).unwrap();
    assert!(!c3.contains("CMAKE_OSX_SYSROOT"));
    assert!(!c3.contains("FLAGS_INIT"));
}

#[test]
fn cmakebuild_scrubbed_env_drops_host_config() {
    // set_var is unsafe in edition 2024 (process-global state); these
    // variables are only read back through scrubbed_env in this test.
    unsafe {
        std::env::set_var("CC", "/evil/cc");
        std::env::set_var("CXX", "/evil/c++");
        std::env::set_var("CPPFLAGS", "-DEVIL");
        std::env::set_var("CFLAGS", "-O0");
        std::env::set_var("CXXFLAGS", "-O0");
        std::env::set_var("LDFLAGS", "-L/evil");
        std::env::set_var("CMAKE_GENERATOR", "Xcode");
        std::env::set_var("CMAKE_PREFIX_PATH", "/evil/prefix");
        std::env::set_var("CPPKG_INTERNAL_TEST", "1");
        std::env::set_var("SDKROOT", "/evil/sdk");
    }

    let env = scrubbed_env();
    for banned in [
        "CC",
        "CXX",
        "CPPFLAGS",
        "CFLAGS",
        "CXXFLAGS",
        "LDFLAGS",
        "CMAKE_GENERATOR",
        "CMAKE_PREFIX_PATH",
        "CPPKG_INTERNAL_TEST",
        "SDKROOT",
    ] {
        assert!(!env.contains_key(banned), "{banned} must be scrubbed");
    }
    assert!(env.contains_key("PATH"), "PATH must survive scrubbing");
    assert!(env.contains_key("HOME"), "HOME must survive scrubbing");
}

/// End-to-end against real cmake/ninja/clang: build+install the fixture,
/// consume it from a second project via CMAKE_PREFIX_PATH, and check the
/// translated version-rejection error against the same install.
#[test]
fn cmakebuild_end_to_end_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let tc = test_toolchain();
    let spec = dummy_spec(BTreeMap::new());

    // 1. Build + install the hello package.
    let src = tmp.path().join("hello-src");
    write_hello_fixture(&src);
    let entry = tmp.path().join("store-entry");
    let req = DepBuildRequest {
        dep_key: "hello",
        spec: &spec,
        source_dir: &src,
        config_hash: "deadbeef",
        entry_dir: &entry,
        config: BuildConfig::Release,
        toolchain: &tc,
        abi_flags: &[],
        prefix_path: &[],
    };
    let built = build_dependency(&req).expect("fixture build should succeed");
    assert_eq!(built.dep_key, "hello");
    assert_eq!(built.config_hash, "deadbeef");
    assert_eq!(built.install_dir, entry.join("install"));
    assert!(built.install_dir.join("lib/libhello.a").exists());
    assert!(built.install_dir.join("include/hello.h").exists());
    assert!(built
        .install_dir
        .join("lib/cmake/hello/helloConfig.cmake")
        .exists());
    assert!(built
        .install_dir
        .join("lib/cmake/hello/helloTargets.cmake")
        .exists());
    assert!(
        !entry.join("build-tmp").exists(),
        "build-tmp must be deleted on success"
    );

    // 2. Consume via find_package + CMAKE_PREFIX_PATH (transitive-provision
    //    plumbing) and link the exported target.
    let consumer_src = tmp.path().join("consumer-src");
    write_file(
        &consumer_src.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.15)
project(consumer LANGUAGES CXX)
find_package(hello REQUIRED)
add_executable(consumer main.cpp)
target_link_libraries(consumer PRIVATE hello::hello)
install(TARGETS consumer RUNTIME DESTINATION bin)
"#,
    );
    write_file(
        &consumer_src.join("main.cpp"),
        "#include \"hello.h\"\nint main() { return hello_add(2, -2); }\n",
    );
    let consumer_entry = tmp.path().join("consumer-entry");
    let prefixes = vec![built.install_dir.clone()];
    let consumer_req = DepBuildRequest {
        dep_key: "consumer",
        spec: &spec,
        source_dir: &consumer_src,
        config_hash: "beefcafe",
        entry_dir: &consumer_entry,
        config: BuildConfig::Release,
        toolchain: &tc,
        abi_flags: &[],
        prefix_path: &prefixes,
    };
    let consumer_built = build_dependency(&consumer_req).expect("consumer build should succeed");
    assert!(consumer_built.install_dir.join("bin/consumer").exists());

    // 3. Version rejection against the same install: requested 99.0 vs
    //    available 1.0.0, translated with both versions named.
    let rejecting_src = tmp.path().join("rejecting-src");
    write_file(
        &rejecting_src.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.15)
project(rejecting LANGUAGES CXX)
find_package(hello 99.0 REQUIRED)
"#,
    );
    let rejecting_entry = tmp.path().join("rejecting-entry");
    let rejecting_req = DepBuildRequest {
        dep_key: "rejecting",
        spec: &spec,
        source_dir: &rejecting_src,
        config_hash: "0000",
        entry_dir: &rejecting_entry,
        config: BuildConfig::Release,
        toolchain: &tc,
        abi_flags: &[],
        prefix_path: &prefixes,
    };
    let err = build_dependency(&rejecting_req).expect_err("version mismatch must fail");
    let msg = format!("{err}");
    assert!(msg.contains("\"hello\""), "must name the package: {msg}");
    assert!(msg.contains("99.0"), "must name the requested version: {msg}");
    assert!(msg.contains("1.0.0"), "must name the available version: {msg}");
    assert!(
        msg.contains("cppkg-configure.log"),
        "must include log path: {msg}"
    );
}

/// Failing-configure fixture: find_package of a package that cannot exist
/// must produce the translated not-found error and keep the log on disk.
#[test]
fn cmakebuild_missing_find_package_is_translated() {
    let tmp = tempfile::tempdir().unwrap();
    let tc = test_toolchain();
    let spec = dummy_spec(BTreeMap::new());

    let src = tmp.path().join("broken-src");
    write_file(
        &src.join("CMakeLists.txt"),
        r#"cmake_minimum_required(VERSION 3.15)
project(broken LANGUAGES CXX)
find_package(CppkgNoSuchPkg REQUIRED)
"#,
    );
    let entry = tmp.path().join("entry");
    let req = DepBuildRequest {
        dep_key: "broken",
        spec: &spec,
        source_dir: &src,
        config_hash: "ffff",
        entry_dir: &entry,
        config: BuildConfig::Release,
        toolchain: &tc,
        abi_flags: &[],
        prefix_path: &[],
    };
    let err = build_dependency(&req).expect_err("configure must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("\"CppkgNoSuchPkg\""),
        "must name the missing package: {msg}"
    );
    assert!(
        msg.contains("[dependencies]"),
        "must point at [dependencies]: {msg}"
    );
    assert!(msg.contains("needs"), "must point at needs: {msg}");
    let log = entry.join("build-tmp/cppkg-configure.log");
    assert!(
        msg.contains(&log.display().to_string()),
        "must include log path: {msg}"
    );
    assert!(log.exists(), "raw configure log must be preserved on failure");
}
