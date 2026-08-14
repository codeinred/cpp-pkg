//! Behavior tests for cppkg::ninja_gen: golden fragments of the emitted
//! build.ninja, compile_commands.json == unit_argv, and a real end-to-end
//! ninja build of a tiny two-target plan.

use std::path::Path;

use cppkg::graph::{BuildPlan, CompileUnit, LinkInput, PlannedTarget};
use cppkg::ninja_gen::{run_ninja, unit_argv, write_compile_commands, write_ninja};
use cppkg::schema::{BuildConfig, TargetKind};
use cppkg::toolchain::{Dialect, Driver, Lang, Toolchain, ToolchainIdentity};

/// Standalone GNU-dialect driver so these tests do not depend on the real
/// toolchain module being implemented.
struct TestDriver;

impl Driver for TestDriver {
    fn std_flag(&self, lang: Lang, std: u32) -> cppkg::Result<String> {
        Ok(match lang {
            Lang::Cxx => format!("-std=c++{std}"),
            Lang::C => format!("-std=c{std}"),
        })
    }
    fn include_args(&self, path: &Path, system: bool) -> Vec<String> {
        if system {
            vec!["-isystem".into(), path.to_string_lossy().into_owned()]
        } else {
            vec![format!("-I{}", path.display())]
        }
    }
    fn define_arg(&self, key: &str, value: Option<&str>) -> String {
        match value {
            Some(v) => format!("-D{key}={v}"),
            None => format!("-D{key}"),
        }
    }
    fn depfile_args(&self, object: &Path, depfile: &Path) -> Vec<String> {
        vec![
            "-MD".into(),
            "-MT".into(),
            object.to_string_lossy().into_owned(),
            "-MF".into(),
            depfile.to_string_lossy().into_owned(),
        ]
    }
    fn sysroot_args(&self, sdk: Option<&Path>) -> Vec<String> {
        match sdk {
            Some(p) => vec!["-isysroot".into(), p.to_string_lossy().into_owned()],
            None => vec![],
        }
    }
    fn framework_args(&self, name: &str) -> Vec<String> {
        vec!["-framework".into(), name.into()]
    }
    fn config_compile_flags(&self, config: BuildConfig) -> Vec<String> {
        let flags: &[&str] = match config {
            BuildConfig::Debug => &["-g"],
            BuildConfig::Release => &["-O3", "-DNDEBUG"],
            BuildConfig::RelWithDebInfo => &["-O2", "-g", "-DNDEBUG"],
            BuildConfig::MinSizeRel => &["-Os", "-DNDEBUG"],
        };
        flags.iter().map(|s| s.to_string()).collect()
    }
}

fn test_toolchain() -> Toolchain {
    Toolchain {
        cxx: "/usr/bin/c++".into(),
        cc: "/usr/bin/cc".into(),
        ar: "/usr/bin/ar".into(),
        sdk_path: None,
        identity: ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id: "AppleClang".into(),
            version: "21.0.0".into(),
            target_triple: "arm64-apple-darwin".into(),
            stdlib: "libc++".into(),
            stdlib_version: "190100".into(),
            sdk_version: None,
        },
    }
}

fn unit(source: &str, object: &str) -> CompileUnit {
    CompileUnit {
        source: source.into(),
        lang: Lang::Cxx,
        std: Some(20),
        includes: vec![],
        defines: vec![],
        extra_flags: vec![],
        object: object.into(),
    }
}

fn exe(name: &str, units: Vec<CompileUnit>, link_inputs: Vec<LinkInput>) -> PlannedTarget {
    PlannedTarget {
        name: name.into(),
        kind: TargetKind::Executable,
        units,
        output: name.into(),
        link_inputs,
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec![],
    }
}

fn rendered(plan: &BuildPlan, config: BuildConfig, build_dir: &Path) -> String {
    write_ninja(plan, &test_toolchain(), &TestDriver, config, build_dir).unwrap();
    std::fs::read_to_string(build_dir.join("build.ninja")).unwrap()
}

#[test]
fn ninja_golden_fragments_escaping_and_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let mut u = unit("/tmp/my src/foo bar.cpp", "obj/we:ird name.o");
    u.includes = vec![("/inc dir".into(), false), ("/sys inc".into(), true)];
    u.defines = vec![("FOO".into(), Some("1".into())), ("BAR".into(), None)];
    let plan = BuildPlan {
        targets: vec![exe(
            "app",
            vec![u],
            vec![LinkInput::Object("obj/we:ird name.o".into())],
        )],
    };
    let text = rendered(&plan, BuildConfig::Release, tmp.path());

    // Build-statement paths use ninja escaping ('$ ', '$:').
    assert!(
        text.contains("build obj/we$:ird$ name.o: cxx_compile /tmp/my$ src/foo$ bar.cpp"),
        "missing escaped build statement in:\n{text}"
    );
    // Header-dependency machinery.
    assert!(text.contains("  deps = gcc"));
    assert!(text.contains("  depfile = $out.d"));
    assert!(text.contains("-MD -MT $out -MF $out.d -c $in -o $out"));
    // Per-build variables, shell-quoted where needed.
    assert!(text.contains("  flags = -std=c++20 -O3 -DNDEBUG"));
    assert!(text.contains("  includes = '-I/inc dir' -isystem '/sys inc'"));
    assert!(text.contains("  defines = -DFOO=1 -DBAR"));
    // Archive rule: rm first (ar q appends), qc create, `ar s` as ranlib.
    assert!(
        text.contains("  command = rm -f $out && /usr/bin/ar qc $out $in && /usr/bin/ar s $out")
    );
    // Executable named like its output gets no phony, and is the default.
    assert!(!text.contains("build app: phony"));
    assert!(text.contains("default app"));
}

#[test]
fn ninja_libs_preserve_plan_order() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = BuildPlan {
        targets: vec![exe(
            "app",
            vec![unit("/s/main.cpp", "obj/main.o")],
            vec![
                LinkInput::Object("obj/main.o".into()),
                LinkInput::SystemLib("z".into()),
                LinkInput::Archive("/store/libbar.a".into()),
                LinkInput::Framework("CoreFoundation".into()),
                LinkInput::SystemLib("m".into()),
                LinkInput::Archive("/store/libfoo.a".into()),
            ],
        )],
    };
    let text = rendered(&plan, BuildConfig::Release, tmp.path());
    // Objects are the only explicit inputs; archives ride $libs (at their
    // plan positions, interleaved with -l/-framework — the topological order
    // graph produced) and reappear as implicit deps for scheduling.
    assert!(
        text.contains("build app: link_cxx obj/main.o | /store/libbar.a /store/libfoo.a"),
        "missing link statement in:\n{text}"
    );
    assert!(
        text.contains(
            "  libs = -lz /store/libbar.a -framework CoreFoundation -lm /store/libfoo.a"
        ),
        "plan order not preserved in $libs:\n{text}"
    );
}

#[test]
fn ninja_sibling_outputs_alias_and_phony() {
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = tmp.path().join("build");
    let lib = PlannedTarget {
        name: "core".into(),
        kind: TargetKind::StaticLibrary,
        units: vec![unit("/s/core.cpp", "obj/core/core.o")],
        output: "libcore.a".into(),
        link_inputs: vec![LinkInput::Object("obj/core/core.o".into())],
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec![],
    };
    let mut app = exe(
        "app",
        vec![unit("/s/main.cpp", "obj/app/main.o")],
        vec![
            LinkInput::Object("obj/app/main.o".into()),
            // Absolute spelling of a sibling output must be rewritten to the
            // sibling's own output node, or ninja would demand the file
            // pre-exist on the first build.
            LinkInput::Archive(build_dir.join("libcore.a")),
        ],
    );
    app.target_deps = vec!["core".into()];
    let plan = BuildPlan {
        targets: vec![lib, app],
    };
    let text = rendered(&plan, BuildConfig::Debug, &build_dir);
    assert!(text.contains("build libcore.a: archive obj/core/core.o"));
    assert!(
        text.contains("build app: link_cxx obj/app/main.o | libcore.a"),
        "sibling alias/implicit dep missing in:\n{text}"
    );
    assert!(
        text.contains("  libs = libcore.a"),
        "sibling archive must be linked via $libs:\n{text}"
    );
    assert!(text.contains("build core: phony libcore.a"));
    assert!(text.contains("default core app"));
}

#[test]
fn ninja_compile_commands_matches_unit_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = tmp.path().join("build");
    let mut u = unit("/s/main.cpp", "obj/main.o");
    u.includes = vec![("/inc".into(), false), ("/sysinc".into(), true)];
    u.defines = vec![("FOO".into(), Some("bar baz".into()))];
    u.extra_flags = vec!["-Wall".into()];
    let plan = BuildPlan {
        targets: vec![exe(
            "app",
            vec![u.clone()],
            vec![LinkInput::Object("obj/main.o".into())],
        )],
    };
    let tc = test_toolchain();
    write_compile_commands(&plan, &tc, &TestDriver, BuildConfig::Debug, &build_dir).unwrap();

    let text = std::fs::read_to_string(build_dir.join("compile_commands.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let entries = v.as_array().expect("top-level array");
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["file"], "/s/main.cpp");
    assert_eq!(entry["output"], "obj/main.o");
    assert_eq!(
        entry["directory"],
        std::path::absolute(&build_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    );
    let args: Vec<String> = entry["arguments"]
        .as_array()
        .expect("arguments array")
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        args,
        unit_argv(&u, &tc, &TestDriver, BuildConfig::Debug).unwrap()
    );
    // Spot-check the depfile flags refer to the object's depfile.
    assert!(args
        .windows(2)
        .any(|w| w[0] == "-MF" && w[1] == "obj/main.o.d"));
}

#[test]
fn ninja_end_to_end_builds_and_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("greet.cpp"),
        "const char* greet() { return \"hello from cpp-pkg\"; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.cpp"),
        "#include <cstdio>\nconst char* greet();\nint main() { std::puts(greet()); return 0; }\n",
    )
    .unwrap();
    let build_dir = tmp.path().join("build");

    let lib = PlannedTarget {
        name: "greet".into(),
        kind: TargetKind::StaticLibrary,
        units: vec![CompileUnit {
            source: src.join("greet.cpp"),
            lang: Lang::Cxx,
            std: Some(17),
            includes: vec![],
            defines: vec![],
            extra_flags: vec![],
            object: "obj/greet/greet.o".into(),
        }],
        output: "libgreet.a".into(),
        link_inputs: vec![LinkInput::Object("obj/greet/greet.o".into())],
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec![],
    };
    let app = PlannedTarget {
        name: "hello".into(),
        kind: TargetKind::Executable,
        units: vec![CompileUnit {
            source: src.join("main.cpp"),
            lang: Lang::Cxx,
            std: Some(17),
            includes: vec![],
            defines: vec![],
            extra_flags: vec![],
            object: "obj/hello/main.o".into(),
        }],
        output: "hello".into(),
        link_inputs: vec![
            LinkInput::Object("obj/hello/main.o".into()),
            LinkInput::Archive(build_dir.join("libgreet.a")),
        ],
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec!["greet".into()],
    };
    let plan = BuildPlan {
        targets: vec![lib, app],
    };
    let tc = test_toolchain();

    write_ninja(&plan, &tc, &TestDriver, BuildConfig::Release, &build_dir).unwrap();
    run_ninja(&build_dir, &["hello".to_string()]).unwrap();

    let out = std::process::Command::new(build_dir.join("hello"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "hello from cpp-pkg"
    );
}

#[test]
fn ninja_static_lib_rejects_non_object_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = PlannedTarget {
        name: "core".into(),
        kind: TargetKind::StaticLibrary,
        units: vec![],
        output: "libcore.a".into(),
        link_inputs: vec![LinkInput::SystemLib("z".into())],
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec![],
    };
    let plan = BuildPlan {
        targets: vec![lib],
    };
    let err = write_ninja(
        &plan,
        &test_toolchain(),
        &TestDriver,
        BuildConfig::Release,
        tmp.path(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("non-object link input"));
}
