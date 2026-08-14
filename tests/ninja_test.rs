//! Behavior tests for cppkg::ninja_gen: golden fragments of the emitted
//! build.ninja, compile_commands.json == unit_argv, a real end-to-end ninja
//! build, and the wave-1 surfaces: generate-step edges (cppkg-genexec /
//! cppkg-gen), runtime-data copy edges (cppkg-copy), and the byte-stability
//! guard for featureless plans.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cppkg::graph::{BuildPlan, CompileUnit, DataStage, LinkInput, PlannedGenStep, PlannedTarget};
use cppkg::ninja_gen::{run_ninja, unit_argv, write_compile_commands, write_ninja};
use cppkg::schema::{BuildConfig, GenerateAction, TargetKind};
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

/// Path handed to write_ninja as the cpp-pkg binary; only rendered into the
/// output when the plan has activated generate steps.
const TEST_EXE: &str = "/opt/cpp-pkg-test/cpp-pkg";

// ---------------------------------------------------------------------------
// Constructors. All struct literals live here so a sibling-bundle field
// addition is a one-place fix.

fn unit(source: &str, object: &str) -> CompileUnit {
    CompileUnit {
        source: source.into(),
        lang: Lang::Cxx,
        std: Some(20),
        includes: vec![],
        defines: vec![],
        extra_flags: vec![],
        object: object.into(),
        references_gen: false,
    }
}

fn target(
    name: &str,
    kind: TargetKind,
    output: &str,
    units: Vec<CompileUnit>,
    link_inputs: Vec<LinkInput>,
) -> PlannedTarget {
    PlannedTarget {
        name: name.into(),
        kind,
        units,
        output: output.into(),
        link_inputs,
        link_flags: vec![],
        link_lang: Lang::Cxx,
        target_deps: vec![],
        install: false,
        dev: false,
        test: false,
        public_includes: vec![],
        public_defines: vec![],
        public_flags: vec![],
        public_link_flags: vec![],
        cxx_std: None,
        public_headers: None,
        run: vec![],
        external_deps: BTreeMap::new(),
        local_deps_public: vec![],
        local_deps_private: vec![],
        external_public: vec![],
        external_link_only: vec![],
        runtime_data: vec![],
    }
}

fn exe(name: &str, units: Vec<CompileUnit>, link_inputs: Vec<LinkInput>) -> PlannedTarget {
    target(name, TargetKind::Executable, name, units, link_inputs)
}

fn lib(
    name: &str,
    output: &str,
    units: Vec<CompileUnit>,
    link_inputs: Vec<LinkInput>,
) -> PlannedTarget {
    target(name, TargetKind::StaticLibrary, output, units, link_inputs)
}

fn plan_of(targets: Vec<PlannedTarget>) -> BuildPlan {
    BuildPlan {
        targets,
        gen_steps: vec![],
        data_stages: vec![],
        warnings: vec![],
    }
}

fn command_step(
    name: &str,
    argv: &[&str],
    stdin: Option<&str>,
    inputs: &[&str],
    output: &str,
) -> PlannedGenStep {
    PlannedGenStep {
        name: name.into(),
        action: GenerateAction::Command {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            stdin: stdin.map(|s| s.to_string()),
            stdout: output.into(),
        },
        inputs: inputs.iter().map(PathBuf::from).collect(),
        output: output.into(),
    }
}

fn template_step(name: &str, template: &str, output: &str) -> PlannedGenStep {
    PlannedGenStep {
        name: name.into(),
        action: GenerateAction::Template {
            template: template.into(),
            output: output.into(),
            vars: BTreeMap::new(),
        },
        inputs: vec![],
        output: output.into(),
    }
}

fn rendered(plan: &BuildPlan, config: BuildConfig, build_dir: &Path) -> String {
    write_ninja(
        plan,
        &test_toolchain(),
        &TestDriver,
        config,
        build_dir,
        Path::new(TEST_EXE),
    )
    .unwrap();
    std::fs::read_to_string(build_dir.join("build.ninja")).unwrap()
}

// ---------------------------------------------------------------------------
// v0 behavior (must be preserved byte-for-byte for featureless plans)

#[test]
fn ninja_golden_fragments_escaping_and_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let mut u = unit("/tmp/my src/foo bar.cpp", "obj/we:ird name.o");
    u.includes = vec![("/inc dir".into(), false), ("/sys inc".into(), true)];
    u.defines = vec![("FOO".into(), Some("1".into())), ("BAR".into(), None)];
    let plan = plan_of(vec![exe(
        "app",
        vec![u],
        vec![LinkInput::Object("obj/we:ird name.o".into())],
    )]);
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
    let plan = plan_of(vec![exe(
        "app",
        vec![unit("/s/main.cpp", "obj/main.o")],
        vec![
            LinkInput::Object("obj/main.o".into()),
            LinkInput::SystemLib("z".into()),
            LinkInput::Archive("/store/libbar.a".into()),
            // §1.3 interleaving: a contributor's raw link-flag words follow
            // its archive immediately (--as-needed survival).
            LinkInput::Flag("-lrt".into()),
            LinkInput::Flag("-Wl,--no-as-needed".into()),
            LinkInput::Framework("CoreFoundation".into()),
            LinkInput::SystemLib("m".into()),
            LinkInput::Archive("/store/libfoo.a".into()),
        ],
    )]);
    let text = rendered(&plan, BuildConfig::Release, tmp.path());
    // Objects are the only explicit inputs; archives ride $libs (at their
    // plan positions, interleaved with -l/-framework/raw flag words — the
    // topological order graph produced; wave-1 §1.3 keeps closure link-flag
    // words in this same stream) and reappear as implicit deps for
    // scheduling.
    assert!(
        text.contains("build app: link_cxx obj/main.o | /store/libbar.a /store/libfoo.a"),
        "missing link statement in:\n{text}"
    );
    assert!(
        text.contains(
            "  libs = -lz /store/libbar.a -lrt -Wl,--no-as-needed -framework CoreFoundation -lm /store/libfoo.a"
        ),
        "plan order not preserved in $libs:\n{text}"
    );
}

#[test]
fn ninja_sibling_outputs_alias_and_phony() {
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = tmp.path().join("build");
    let core = lib(
        "core",
        "libcore.a",
        vec![unit("/s/core.cpp", "obj/core/core.o")],
        vec![LinkInput::Object("obj/core/core.o".into())],
    );
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
    let plan = plan_of(vec![core, app]);
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
    let plan = plan_of(vec![exe(
        "app",
        vec![u.clone()],
        vec![LinkInput::Object("obj/main.o".into())],
    )]);
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

    let mut glib = lib(
        "greet",
        "libgreet.a",
        vec![unit(&src.join("greet.cpp").to_string_lossy(), "obj/greet/greet.o")],
        vec![LinkInput::Object("obj/greet/greet.o".into())],
    );
    glib.units[0].std = Some(17);
    let mut app = exe(
        "hello",
        vec![unit(&src.join("main.cpp").to_string_lossy(), "obj/hello/main.o")],
        vec![
            LinkInput::Object("obj/hello/main.o".into()),
            LinkInput::Archive(build_dir.join("libgreet.a")),
        ],
    );
    app.units[0].std = Some(17);
    app.target_deps = vec!["greet".into()];
    let plan = plan_of(vec![glib, app]);
    let tc = test_toolchain();

    write_ninja(
        &plan,
        &tc,
        &TestDriver,
        BuildConfig::Release,
        &build_dir,
        Path::new(TEST_EXE),
    )
    .unwrap();
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
    let core = lib(
        "core",
        "libcore.a",
        vec![],
        vec![LinkInput::SystemLib("z".into())],
    );
    let plan = plan_of(vec![core]);
    let err = write_ninja(
        &plan,
        &test_toolchain(),
        &TestDriver,
        BuildConfig::Release,
        tmp.path(),
        Path::new(TEST_EXE),
    )
    .unwrap_err();
    assert!(err.to_string().contains("non-object link input"));
}

// ---------------------------------------------------------------------------
// Wave 1: generate-step edges (§4.2)

#[test]
fn ninja_gen_edges_rule_phony_and_order_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::path::absolute(tmp.path()).unwrap();
    let build_dir = root.join("build");

    // A generated source: graph interpolated ${gen} to the absolute gen root
    // (<build>/gen) and marked the unit.
    let mut gen_unit = unit(
        &build_dir.join("gen/src/version.cpp").to_string_lossy(),
        "obj/app/version.o",
    );
    gen_unit.references_gen = true;
    // An ordinary unit that only includes gen headers: order-only dep, but
    // its source spelling is untouched.
    let mut inc_unit = unit("/s/main.cpp", "obj/app/main.o");
    inc_unit.references_gen = true;

    let mut plan = plan_of(vec![exe(
        "app",
        vec![gen_unit, inc_unit],
        vec![
            LinkInput::Object("obj/app/version.o".into()),
            LinkInput::Object("obj/app/main.o".into()),
        ],
    )]);
    plan.gen_steps = vec![
        template_step("version-src", "src/version.cpp.in", "src/version.cpp"),
        command_step(
            "browse-py-h",
            &["sh", "src/inline.sh", "kBrowsePy"],
            Some("src/browse.py"),
            &["src/inline.sh"],
            "browse_py.h",
        ),
    ];
    let text = rendered(&plan, BuildConfig::Release, &build_dir);

    // The rule: stable gen-exec argv, restat for unchanged-output pruning.
    assert!(text.contains("rule cppkg-genexec"), "missing rule in:\n{text}");
    assert!(
        text.contains(&format!(
            "  command = {TEST_EXE} gen-exec --project-root {} --step $step --digest $step_digest",
            root.display()
        )),
        "gen-exec argv contract broken in:\n{text}"
    );
    assert!(text.contains("  restat = 1"));
    // Every edge carries a content digest (step_digest enters the expanded
    // command ninja hashes — rebuild correctness for var/argv-only changes).
    assert_eq!(text.matches("  step_digest = ").count(), 2);

    // Template edge: output spelled absolute under <build>/gen (matching
    // compiler depfile spellings, so a dirty gen edge dirties dependents in
    // the same build), template file is an input, step name rides a
    // per-build variable.
    assert!(
        text.contains(&format!(
            "build {}: cppkg-genexec {}",
            build_dir.join("gen/src/version.cpp").display(),
            root.join("src/version.cpp.in").display()
        )),
        "template edge missing in:\n{text}"
    );
    assert!(text.contains("  step = version-src"));

    // Command edge: declared inputs + stdin are inputs.
    assert!(
        text.contains(&format!(
            "build {}: cppkg-genexec {} {}",
            build_dir.join("gen/browse_py.h").display(),
            root.join("src/inline.sh").display(),
            root.join("src/browse.py").display()
        )),
        "command edge missing in:\n{text}"
    );
    assert!(text.contains("  step = browse-py-h"));

    // Phony aggregate over all activated outputs.
    assert!(
        text.contains(&format!(
            "build cppkg-gen: phony {} {}",
            build_dir.join("gen/src/version.cpp").display(),
            build_dir.join("gen/browse_py.h").display()
        )),
        "phony aggregate missing in:\n{text}"
    );

    // Generated source compiles from the gen edge's own spelling and waits
    // on the aggregate; the header-only referencing unit keeps its source
    // spelling but still gets the order-only dep.
    assert!(
        text.contains(&format!(
            "build obj/app/version.o: cxx_compile {} || cppkg-gen",
            build_dir.join("gen/src/version.cpp").display()
        )),
        "generated source not aliased to the gen edge in:\n{text}"
    );
    assert!(
        text.contains("build obj/app/main.o: cxx_compile /s/main.cpp || cppkg-gen"),
        "gen-referencing unit lacks order-only dep in:\n{text}"
    );

    // Gen outputs are pulled by consumers, never by default.
    let default_line = text
        .lines()
        .find(|l| l.starts_with("default "))
        .expect("default line");
    assert!(!default_line.contains("gen/"), "gen outputs leaked into defaults: {default_line}");
    assert!(!default_line.contains("cppkg-gen"));
}

#[test]
fn ninja_gen_interstep_inputs_alias_to_producing_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::path::absolute(tmp.path()).unwrap();
    let build_dir = root.join("build");

    let mut u = unit("/s/main.cpp", "obj/main.o");
    u.references_gen = true;
    let mut plan = plan_of(vec![exe(
        "app",
        vec![u],
        vec![LinkInput::Object("obj/main.o".into())],
    )]);
    // Step B consumes step A's output; graph hands the input through as the
    // interpolated absolute ${gen} path.
    let step_a = command_step("zic", &["zic"], None, &["data/tzdata.zi"], "zoneinfo/UTC");
    let mut step_b = command_step("pack", &["pack"], None, &[], "packed.h");
    step_b.inputs = vec![build_dir.join("gen/zoneinfo/UTC")];
    plan.gen_steps = vec![step_a, step_b];

    let text = rendered(&plan, BuildConfig::Debug, &build_dir);
    assert!(
        text.contains(&format!(
            "build {}: cppkg-genexec {}",
            build_dir.join("gen/packed.h").display(),
            build_dir.join("gen/zoneinfo/UTC").display()
        )),
        "inter-step input must alias to the producing edge's spelling in:\n{text}"
    );
}

#[test]
fn ninja_gen_step_digest_tracks_interpolated_content() {
    // A template step whose only change is an interpolated var value (no
    // input file touched) must change the emitted ninja bytes — the digest
    // rides the expanded command line, so ninja re-runs the step. This is
    // the tier-a staleness guard (e.g. `${package.version}` bumps).
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = std::path::absolute(tmp.path()).unwrap().join("build");

    let render_with_version = |version: &str| {
        let mut u = unit("/s/main.cpp", "obj/main.o");
        u.references_gen = true;
        let mut plan = plan_of(vec![exe(
            "app",
            vec![u],
            vec![LinkInput::Object("obj/main.o".into())],
        )]);
        let mut step = template_step("version-header", "src/version.hpp.in", "src/version.hpp");
        if let GenerateAction::Template { vars, .. } = &mut step.action {
            vars.insert("PROJECT_VERSION".into(), version.into());
        }
        plan.gen_steps = vec![step];
        rendered(&plan, BuildConfig::Release, &build_dir)
    };

    let a = render_with_version("1.4.0");
    let b = render_with_version("1.5.0");
    assert_ne!(a, b, "var-only change must alter the gen edge's command hash input");
    let digest_of = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("  step_digest = "))
            .expect("digest line")
            .to_string()
    };
    assert_ne!(digest_of(&a), digest_of(&b));
}

// ---------------------------------------------------------------------------
// Wave 1: runtime-data copy edges (§6.5)

#[test]
fn ninja_copy_edges_dedupe_and_attach_order_only() {
    let tmp = tempfile::tempdir().unwrap();
    let build_dir = tmp.path().join("build");
    let mut plan = plan_of(vec![
        exe(
            "cppcheck",
            vec![unit("/s/a.cpp", "obj/cppcheck/a.o")],
            vec![LinkInput::Object("obj/cppcheck/a.o".into())],
        ),
        exe(
            "testrunner",
            vec![unit("/s/t.cpp", "obj/testrunner/t.o")],
            vec![LinkInput::Object("obj/testrunner/t.o".into())],
        ),
    ]);
    // Two targets declaring the same byte-equal data: one copy edge, both
    // target output edges wait on it (order-only — data changes never
    // re-link).
    plan.data_stages = vec![
        DataStage {
            src: "/data/cfg/std.cfg".into(),
            dest: "cfg/std.cfg".into(),
            for_targets: vec!["cppcheck".into()],
        },
        DataStage {
            src: "/data/cfg/std.cfg".into(),
            dest: "cfg/std.cfg".into(),
            for_targets: vec!["testrunner".into()],
        },
        DataStage {
            src: "/data/platforms/unix64.xml".into(),
            dest: "platforms/unix64.xml".into(),
            for_targets: vec!["cppcheck".into(), "testrunner".into()],
        },
    ];
    let text = rendered(&plan, BuildConfig::Release, &build_dir);

    assert!(text.contains("rule cppkg-copy"), "missing copy rule in:\n{text}");
    assert!(text.contains("  command = mkdir -p $outdir && cp $in $out"));
    assert_eq!(
        text.matches("build cfg/std.cfg: cppkg-copy /data/cfg/std.cfg").count(),
        1,
        "byte-equal duplicate destinations must emit one edge:\n{text}"
    );
    assert!(text.contains("  outdir = cfg"));
    assert!(text.contains("build platforms/unix64.xml: cppkg-copy /data/platforms/unix64.xml"));
    assert!(
        text.contains("build cppcheck: link_cxx obj/cppcheck/a.o || cfg/std.cfg platforms/unix64.xml"),
        "copy edges not attached order-only to owning target in:\n{text}"
    );
    assert!(
        text.contains("build testrunner: link_cxx obj/testrunner/t.o || cfg/std.cfg platforms/unix64.xml"),
        "copy edges not attached order-only to second owner in:\n{text}"
    );
}

#[test]
fn ninja_copy_edge_conflicting_sources_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mut plan = plan_of(vec![exe(
        "app",
        vec![unit("/s/a.cpp", "obj/a.o")],
        vec![LinkInput::Object("obj/a.o".into())],
    )]);
    plan.data_stages = vec![
        DataStage {
            src: "/data/one/f.cfg".into(),
            dest: "cfg/f.cfg".into(),
            for_targets: vec!["app".into()],
        },
        DataStage {
            src: "/data/two/f.cfg".into(),
            dest: "cfg/f.cfg".into(),
            for_targets: vec!["app".into()],
        },
    ];
    let err = write_ninja(
        &plan,
        &test_toolchain(),
        &TestDriver,
        BuildConfig::Release,
        tmp.path(),
        Path::new(TEST_EXE),
    )
    .unwrap_err();
    assert!(err.to_string().contains("two different sources"), "{err}");
}

#[test]
fn ninja_copy_edges_end_to_end_stage_beside_target() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(src.join("main.cpp"), "int main() { return 0; }\n").unwrap();
    std::fs::write(data.join("std.cfg"), "cfg-bytes\n").unwrap();
    let build_dir = tmp.path().join("build");

    let mut app = exe(
        "app",
        vec![unit(&src.join("main.cpp").to_string_lossy(), "obj/app/main.o")],
        vec![LinkInput::Object("obj/app/main.o".into())],
    );
    app.units[0].std = Some(17);
    let mut plan = plan_of(vec![app]);
    plan.data_stages = vec![DataStage {
        src: data.join("std.cfg"),
        dest: "cfg/std.cfg".into(),
        for_targets: vec!["app".into()],
    }];

    write_ninja(
        &plan,
        &test_toolchain(),
        &TestDriver,
        BuildConfig::Release,
        &build_dir,
        Path::new(TEST_EXE),
    )
    .unwrap();
    // Building the target must stage the data beside it (§6.5: kills the
    // silent zero-findings shape).
    run_ninja(&build_dir, &["app".to_string()]).unwrap();
    let staged = build_dir.join("cfg/std.cfg");
    assert_eq!(std::fs::read_to_string(&staged).unwrap(), "cfg-bytes\n");
}

// ---------------------------------------------------------------------------
// Wave 1: byte-stability guard

#[test]
fn ninja_featureless_plan_is_byte_stable_v0() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = plan_of(vec![exe(
        "app",
        vec![unit("/s/main.cpp", "obj/main.o")],
        vec![LinkInput::Object("obj/main.o".into())],
    )]);
    let text = rendered(&plan, BuildConfig::Release, tmp.path());

    // No wave-1 machinery may appear for a featureless plan…
    for needle in ["cppkg-genexec", "cppkg-gen", "cppkg-copy", "gen-exec", "||", TEST_EXE] {
        assert!(!text.contains(needle), "featureless output contains '{needle}':\n{text}");
    }

    // …and the whole file is pinned byte-for-byte. If this golden changes,
    // every user's first post-upgrade build relinks the world; change it
    // only deliberately. (Deliberate delta from the v0 emission, wave-1 fix
    // pass: the link rule moved $link_flags after `-o $out $in` per §1.3 —
    // objects precede [flags]/profile link-flags. One-time relink,
    // release-noted; store keys untouched.)
    let expected = "\
# Generated by cpp-pkg; regenerated on every build — do not edit.
ninja_required_version = 1.5

rule cxx_compile
  command = /usr/bin/c++ $flags $includes $defines -MD -MT $out -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
  description = CXX $out

rule c_compile
  command = /usr/bin/cc $flags $includes $defines -MD -MT $out -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
  description = CC $out

rule archive
  command = rm -f $out && /usr/bin/ar qc $out $in && /usr/bin/ar s $out
  description = AR $out

rule link_cxx
  command = /usr/bin/c++ -o $out $in $link_flags $libs
  description = LINK $out

rule link_c
  command = /usr/bin/cc -o $out $in $link_flags $libs
  description = LINK $out

build obj/main.o: cxx_compile /s/main.cpp
  flags = -std=c++20 -O3 -DNDEBUG
build app: link_cxx obj/main.o

default app
";
    assert_eq!(text, expected, "featureless build.ninja drifted from the v0 golden");
}
