//! CLI + build orchestration (per CPP_PKG.md CLI section).
//!
//! Wave-1 surface:
//!   cpp-pkg build [TARGETS...] [--config <c>] [--toolchain <t>]
//!     [--prefix <p>] [--allow-undeclared-system-libs] [--query [PATH]]
//!   cpp-pkg test [FILTER...] [--config] [--toolchain] [--jobs N] [--list]
//!     [--verbose] [-- PASSTHROUGH...]
//!   cpp-pkg install --prefix <dir> [--destdir <dir>] [--config]
//!     [--toolchain] [--list] [TARGETS...]
//!   cpp-pkg gen [--check]          (checked-in [generate] steps)
//!   cpp-pkg gen-exec ...           (hidden; invoked by ninja gen edges)
//!   cpp-pkg provider-script / provide (v0, unchanged)
//!
//! Orchestration pipeline for `build` (each step's module owns its logic —
//! this module only sequences and reports):
//!   1. schema::load(CppPkg.toml) -> print warnings
//!   2. toolchain detect + cfg truth; effective profile ([flags] ABI words
//!      injected at the head of the profile layer, spec §1.3 layer 2)
//!   3. lockfile: eager locking of EVERY declared dependency (§3.2 —
//!      dev and cfg-inactive alike; system deps as declarations)
//!   4. provisioning: graph::required_deps filters the declared universe;
//!      fetched deps run fetch(+patches) -> config hash (+subdir) -> cmake
//!      build -> probe -> manifest; system deps run the sysdep probe into
//!      a manifest-only store entry (§5.3); every manifest read passes the
//!      hermeticity scan (§5.5)
//!   5. graph::plan (cfg truth + interp ctx + request) ->
//!      ninja_gen::write_ninja/write_compile_commands -> run_ninja
//!
//! `provide` reuses the pipeline restricted to the requested package + its
//! `needs` closure, emits a Config.cmake shim, and prints the shim directory
//! as its ONLY stdout output.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};

use anyhow::{Context, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::Result;
use crate::cmake_build::{self, DepBuildRequest, SysdepAllow};
use crate::graph::{self, BuildPlan, BuildRequest, PlannedTarget};
use crate::hashing::{self, ConfigHashInputs, SysdepHashInputs};
use crate::interp::{self, InterpCtx, InterpPos, PinInfo};
use crate::lockfile::{self, LockedPackage, Lockfile};
use crate::manifest::{self, HermeticityAllow, Manifest};
use crate::probe::{self, SysdepFacts};
use crate::schema::{
    self, BuildConfig, CfgTruth, DependencySpec, GenerateAction, GenerateStep, GitRef,
    PackageFlags, Profile, ProjectFile, SourceSpec, Warnings,
};
use crate::shim::{self, ExportInputs, ExportTarget, InstallRequest};
use crate::store::Stores;
use crate::toolchain::{self, GnuDriver, Toolchain};
use crate::{fetch, ninja_gen};

#[derive(Parser)]
#[command(
    name = "cpp-pkg",
    version,
    about = "A C++ package manager and build system consuming CMake dependencies"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the project in the current directory (and its dependencies).
    Build {
        /// Targets to build (default: all non-dev targets).
        targets: Vec<String>,
        /// Build configuration: debug, release, relwithdebinfo, minsizerel.
        #[arg(long, default_value = "release")]
        config: String,
        /// A [toolchains] preset name, or a C++ compiler path/command.
        #[arg(long)]
        toolchain: Option<String>,
        /// Value of ${install-prefix} in define interpolation (default
        /// /usr/local); `cpp-pkg install --prefix` sets it consistently.
        #[arg(long)]
        prefix: Option<String>,
        /// Downgrade hermeticity-scan hits (undeclared absolute system
        /// paths in dependency manifests) from errors to warnings.
        /// Documented as unsupported-for-sharing.
        #[arg(long)]
        allow_undeclared_system_libs: bool,
        /// Print compile commands instead of building; with PATH, only the
        /// command for that source file.
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
        query: Option<String>,
    },
    /// Build and run test targets (`test = true`) and their [[run]] entries.
    Test {
        /// Substring filters on test target names (empty: all tests).
        filters: Vec<String>,
        #[arg(long, default_value = "release")]
        config: String,
        #[arg(long)]
        toolchain: Option<String>,
        /// Parallel test invocations (default 1: serial — shared fixture
        /// cwds make parallel the unsafe default).
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// List the selected invocations without building or running.
        #[arg(long)]
        list: bool,
        /// Replay captured output for passing invocations too.
        #[arg(long)]
        verbose: bool,
        #[arg(long)]
        allow_undeclared_system_libs: bool,
        /// Extra arguments appended to every selected invocation.
        #[arg(last = true)]
        passthrough: Vec<String>,
    },
    /// Build, then stage an FHS install (bin/ lib/ include/ share/
    /// lib/cmake/<Name>/) under --prefix (spec §6.2).
    Install {
        /// Targets to install (default: all `install = true` targets).
        targets: Vec<String>,
        /// Install prefix baked into ${install-prefix} and the layout root.
        #[arg(long)]
        prefix: PathBuf,
        /// Stage into <destdir><prefix> while rendered files refer to
        /// <prefix> (distro packaging; DESTDIR env is honored too).
        #[arg(long)]
        destdir: Option<PathBuf>,
        #[arg(long, default_value = "release")]
        config: String,
        #[arg(long)]
        toolchain: Option<String>,
        /// Print the staging plan without building or writing.
        #[arg(long)]
        list: bool,
        #[arg(long)]
        allow_undeclared_system_libs: bool,
    },
    /// Regenerate checked-in [generate] outputs (the one sanctioned
    /// source-tree write); --check byte-diffs instead (CI mode).
    Gen {
        /// Verify checked-in outputs are current; nonzero exit on drift.
        #[arg(long)]
        check: bool,
    },
    /// Internal: execute one activated [generate] step (invoked by ninja).
    #[command(hide = true, name = "gen-exec")]
    GenExec {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        step: String,
        /// Step content digest; rides the ninja command line so edges
        /// re-run when the step definition changes. Informational here.
        #[arg(long)]
        digest: Option<String>,
    },
    /// Emit the CMake dependency-provider script (cppkg_provider.cmake) into
    /// DIR, with this cpp-pkg binary's path baked in. Use it from a CMake
    /// consumer via -DCMAKE_PROJECT_TOP_LEVEL_INCLUDES=<emitted path>.
    ProviderScript {
        /// Directory to write cppkg_provider.cmake into.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Internal: resolve one package for the CMake dependency provider and
    /// print the emitted Config.cmake shim directory on stdout.
    #[command(hide = true)]
    Provide {
        /// The find_package() name the consumer requested.
        #[arg(long)]
        package: String,
        /// Consumer project directory (contains CppPkg.toml).
        #[arg(long)]
        project: PathBuf,
        /// Build configuration for the provided package.
        #[arg(long, default_value = "release")]
        config: String,
    },
}

pub fn run() -> Result<()> {
    match Cli::parse().command {
        Cmd::Build {
            targets,
            config,
            toolchain,
            prefix,
            allow_undeclared_system_libs,
            query,
        } => build(
            &targets,
            &config,
            toolchain.as_deref(),
            prefix.as_deref(),
            allow_undeclared_system_libs,
            query.as_deref(),
        ),
        Cmd::Test {
            filters,
            config,
            toolchain,
            jobs,
            list,
            verbose,
            allow_undeclared_system_libs,
            passthrough,
        } => test(
            &filters,
            &config,
            toolchain.as_deref(),
            jobs.max(1),
            list,
            verbose,
            allow_undeclared_system_libs,
            &passthrough,
        ),
        Cmd::Install {
            targets,
            prefix,
            destdir,
            config,
            toolchain,
            list,
            allow_undeclared_system_libs,
        } => install(
            &targets,
            &prefix,
            destdir.as_deref(),
            &config,
            toolchain.as_deref(),
            list,
            allow_undeclared_system_libs,
        ),
        Cmd::Gen { check } => gen_verb(check),
        Cmd::GenExec {
            project_root,
            step,
            digest: _,
        } => gen_exec(&project_root, &step),
        Cmd::ProviderScript { dir } => provider_script(&dir),
        Cmd::Provide {
            package,
            project,
            config,
        } => provide(&package, &project, &config),
    }
}

/// `provider-script`: emit cppkg_provider.cmake and print its path. The
/// script bakes in this binary's absolute path, so it is machine-specific
/// and should be regenerated rather than committed.
fn provider_script(dir: &Path) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the cpp-pkg binary path")?;
    let path = shim::write_provider_script(dir, &bin)?;
    println!("{}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Session: everything the verbs share before planning.

struct Session {
    root: PathBuf,
    project: ProjectFile,
    config: BuildConfig,
    toolchain: Toolchain,
    truth: CfgTruth,
    /// Selected profile with `[flags]` ABI words injected at its head
    /// (spec §1.3 layer 2; graph strips them from layer 3).
    profile: Profile,
    abi_flags: Vec<String>,
}

fn open_session(config_key: &str, toolchain_arg: Option<&str>) -> Result<Session> {
    let root = std::env::current_dir().context("determining current directory")?;
    let (project, warnings) = schema::load(&root.join("CppPkg.toml"))?;
    print_warnings(&warnings);

    let config = BuildConfig::from_key(config_key)?;
    let toolchain = select_toolchain(&project, toolchain_arg)?;
    let truth = toolchain.identity.cfg_truth()?;
    let base_profile = project
        .profiles
        .get(config.key())
        .cloned()
        .unwrap_or_default();
    let profile = effective_profile(&project.flags, &truth, &base_profile);
    let abi_flags = profile_abi_flags(&profile);
    Ok(Session {
        root,
        project,
        config,
        toolchain,
        truth,
        profile,
        abi_flags,
    })
}

/// Inject ABI-classified `[flags]` words (unconditional + matching cfg
/// groups, per list) at the HEAD of the corresponding profile lists.
///
/// Spec §1.3 puts the ABI injection set at layer 2, before the `[flags]`
/// non-ABI remainder; emitting via the profile layer places these words one
/// layer later. The sets are disjoint flag families (ABI words never appear
/// in the stripped non-ABI layer), so no last-wins pair straddles the gap —
/// and routing through the profile reuses the entire v0 ABI machinery
/// (compile emission, toolchain-file injection, dependency config hashes)
/// without a second channel. Deviation recorded in the wave report.
fn effective_profile(flags: &PackageFlags, truth: &CfgTruth, profile: &Profile) -> Profile {
    let abi_of = |base: &Vec<String>, from_group: fn(&schema::PackageFlagsGroup) -> &Vec<String>| {
        let mut words: Vec<String> = base.clone();
        for (pred, group) in &flags.cfg {
            if pred.eval(truth) {
                words.extend(from_group(group).iter().cloned());
            }
        }
        abi_classified_words(&words)
    };
    let mut cxx = abi_of(&flags.cxx_flags, |g| &g.cxx_flags);
    let mut c = abi_of(&flags.c_flags, |g| &g.c_flags);
    let mut link = abi_of(&flags.link_flags, |g| &g.link_flags);
    cxx.extend(profile.cxx_flags.iter().cloned());
    c.extend(profile.c_flags.iter().cloned());
    link.extend(profile.link_flags.iter().cloned());
    Profile {
        cxx_flags: cxx,
        c_flags: c,
        link_flags: link,
    }
}

/// The ABI-classified words of a flag list, original spellings, argv order
/// (both words of a two-argv transport pair).
fn abi_classified_words(words: &[String]) -> Vec<String> {
    let classified = toolchain::classify_word_sequence(words);
    let mut keep = vec![false; words.len()];
    for c in &classified {
        if matches!(c.class, toolchain::FlagClass::Abi) && c.index < keep.len() {
            keep[c.index] = true;
        }
    }
    words
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, w)| w.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// `build`

fn build(
    targets: &[String],
    config_key: &str,
    toolchain_arg: Option<&str>,
    prefix: Option<&str>,
    allow_undeclared: bool,
    query: Option<&str>,
) -> Result<()> {
    let sess = open_session(config_key, toolchain_arg)?;
    let request = if targets.is_empty() {
        BuildRequest::Default
    } else {
        BuildRequest::Named(targets.to_vec())
    };
    let pb = plan_and_write(&sess, &request, prefix, allow_undeclared)?;
    if pb.plan.targets.is_empty() {
        eprintln!("cpp-pkg: no targets to build (dependencies are up to date)");
        return Ok(());
    }

    if let Some(filter) = query {
        let driver = GnuDriver;
        return print_query(
            &pb.plan,
            &sess.toolchain,
            &driver,
            sess.config,
            &sess.root,
            filter,
        );
    }
    ninja_gen::run_ninja(&pb.build_dir, targets)
}

struct PlannedBuild {
    plan: BuildPlan,
    deps: DepArtifacts,
    build_dir: PathBuf,
}

/// Provision dependencies (lazily, per the request), plan, and write the
/// ninja file + compile_commands. Shared by build/test/install.
fn plan_and_write(
    sess: &Session,
    request: &BuildRequest,
    install_prefix: Option<&str>,
    allow_undeclared: bool,
) -> Result<PlannedBuild> {
    let required = graph::required_deps(&sess.project, &sess.truth, request)?;
    let deps = prepare_dependencies(
        &sess.project,
        &sess.root,
        &sess.toolchain,
        sess.config,
        &sess.abi_flags,
        DepScope::Build {
            required: &required,
        },
        allow_undeclared,
    )?;

    let build_dir = sess.root.join("build");
    let gen_root = build_dir.join("gen");
    let ictx = InterpCtx {
        package_name: &sess.project.package.name,
        package_version: sess.project.package.version.as_deref(),
        pins: &deps.pins,
        gen_root: Some(&gen_root),
        project_root: Some(&sess.root),
        build_dir: Some(&build_dir),
        install_prefix,
    };
    let plan = graph::plan(&graph::PlanInputs {
        project: &sess.project,
        project_root: &sess.root,
        manifests: &deps.manifests,
        config: sess.config,
        profile: &sess.profile,
        cfg_truth: &sess.truth,
        request,
        interp: &ictx,
    })?;
    for w in &plan.warnings {
        eprintln!("cpp-pkg: warning: {w}");
    }

    if !plan.targets.is_empty() {
        let driver = GnuDriver;
        let cpp_pkg_exe =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cpp-pkg"));
        ninja_gen::write_ninja(
            &plan,
            &sess.toolchain,
            &driver,
            sess.config,
            &build_dir,
            &cpp_pkg_exe,
        )?;
        ninja_gen::write_compile_commands(
            &plan,
            &sess.toolchain,
            &driver,
            sess.config,
            &build_dir,
        )?;
    }

    Ok(PlannedBuild {
        plan,
        deps,
        build_dir,
    })
}

/// `--query`: print the exact compile command(s) ninja would run, without
/// building. An empty filter means every unit; otherwise only units whose
/// source matches the given path (absolute, project-relative, or suffix).
fn print_query(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &GnuDriver,
    config: BuildConfig,
    root: &Path,
    filter: &str,
) -> Result<()> {
    let wanted: Option<PathBuf> = if filter.is_empty() {
        None
    } else {
        let p = Path::new(filter);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        // Canonicalize so `src/../src/main.cpp` and symlinked paths (macOS
        // /tmp vs /private/tmp) still match the plan's expanded sources.
        Some(abs.canonicalize().unwrap_or(abs))
    };

    let mut printed = false;
    for target in &plan.targets {
        for unit in &target.units {
            if let Some(want) = &wanted {
                let source = unit.source.canonicalize().unwrap_or(unit.source.clone());
                if &source != want {
                    continue;
                }
            }
            let argv = ninja_gen::unit_argv(unit, toolchain, driver, config)?;
            let line: Vec<String> = argv.iter().map(|w| shell_word(w)).collect();
            println!("{}", line.join(" "));
            printed = true;
        }
    }
    if !printed && !filter.is_empty() {
        bail!("--query: no compile unit found for '{filter}'");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `test` (spec §3.2)

#[allow(clippy::too_many_arguments)]
fn test(
    filters: &[String],
    config_key: &str,
    toolchain_arg: Option<&str>,
    jobs: usize,
    list: bool,
    verbose: bool,
    allow_undeclared: bool,
    passthrough: &[String],
) -> Result<()> {
    let sess = open_session(config_key, toolchain_arg)?;
    let request = BuildRequest::Tests(filters.to_vec());
    // FILTER-matches-nothing errors surface here (graph::request_seeds).
    let pb = plan_and_write(&sess, &request, None, allow_undeclared)?;

    let test_targets: Vec<&PlannedTarget> =
        pb.plan.targets.iter().filter(|t| t.test).collect();
    if test_targets.is_empty() {
        eprintln!("cpp-pkg: no test targets");
        return Ok(());
    }

    let invocations = collect_invocations(&sess.root, &pb.build_dir, &test_targets, passthrough);
    if list {
        for inv in &invocations {
            println!("{}", inv.describe());
        }
        return Ok(());
    }

    // §4.2 laziness: a test run activates fixture-only [generate] steps that
    // no compile unit depends on (run-entry env/args referencing ${gen} —
    // vtz's zoneinfo). ninja's default set deliberately excludes cppkg-gen,
    // so request it explicitly alongside the planned targets whenever the
    // plan activated any step; otherwise the fixture is silently missing.
    let ninja_targets: Vec<String> = if pb.plan.gen_steps.is_empty() {
        Vec::new() // defaults
    } else {
        pb.plan
            .targets
            .iter()
            .map(|t| t.name.clone())
            .chain(std::iter::once("cppkg-gen".to_string()))
            .collect()
    };
    ninja_gen::run_ninja(&pb.build_dir, &ninja_targets)?;
    run_invocations(&sess.root, invocations, jobs, verbose)
}

/// One test-process invocation, fully resolved (interp already applied by
/// graph; ${build-dir} etc. are final).
struct Invocation {
    target: String,
    label: String,
    exe: PathBuf,
    args: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    env_remove: Vec<String>,
    expect_failure: bool,
}

impl Invocation {
    fn describe(&self) -> String {
        let mut s = format!("{}", self.exe.display());
        for a in &self.args {
            s.push(' ');
            s.push_str(&shell_word(a));
        }
        if let Some(cwd) = &self.cwd {
            s.push_str(&format!("  (cwd: {cwd})"));
        }
        format!("{}: {s}", self.label)
    }
}

fn collect_invocations(
    _root: &Path,
    build_dir: &Path,
    targets: &[&PlannedTarget],
    passthrough: &[String],
) -> Vec<Invocation> {
    let mut out = Vec::new();
    for t in targets {
        let exe = build_dir.join(&t.output);
        // Zero declared entries = one default invocation (§3.2).
        let default_entry = [schema::RunEntry::default()];
        let entries: &[schema::RunEntry] = if t.run.is_empty() {
            &default_entry
        } else {
            &t.run
        };
        for (i, e) in entries.iter().enumerate() {
            let label = match &e.name {
                Some(n) => format!("{} [{n}]", t.name),
                None if entries.len() == 1 => t.name.clone(),
                None => format!("{} [#{i}]", t.name),
            };
            let mut args = e.args.clone();
            args.extend(passthrough.iter().cloned());
            out.push(Invocation {
                target: t.name.clone(),
                label,
                exe: exe.clone(),
                args,
                cwd: e.cwd.clone(),
                env: e.env.clone(),
                env_remove: e.env_remove.clone(),
                expect_failure: e.expect_failure,
            });
        }
    }
    out
}

struct RunOutcome {
    passed: bool,
    detail: String,
    /// Captured stdout+stderr, replayed on failure (or with --verbose).
    output: String,
}

fn run_one(root: &Path, inv: &Invocation) -> RunOutcome {
    // cwd rule (§3.2): relative to the project root; created iff it
    // resolves inside the top-level build tree; otherwise it must exist —
    // a missing fixture cwd fails this invocation legibly.
    let cwd = match &inv.cwd {
        None => root.to_path_buf(),
        Some(c) => {
            let p = Path::new(c);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            };
            // Lexically resolve `.`/`..` before the inside-build-tree check:
            // `build/../../elsewhere` must not pass a raw starts_with and get
            // auto-created outside the project ("resolves inside", §3.2).
            let abs = lexical_normalize(&abs);
            if !abs.is_dir() {
                if abs.starts_with(lexical_normalize(&root.join("build"))) {
                    if let Err(e) = fs::create_dir_all(&abs) {
                        return RunOutcome {
                            passed: false,
                            detail: format!("cannot create cwd {}: {e}", abs.display()),
                            output: String::new(),
                        };
                    }
                } else {
                    return RunOutcome {
                        passed: false,
                        detail: format!(
                            "cwd `{c}` does not exist (fixture directories \
                             outside build/ must be present)"
                        ),
                        output: String::new(),
                    };
                }
            }
            abs
        }
    };

    // env rule (§3.2): inherit -> remove -> set (set-to-"" and remove are
    // distinct).
    let mut cmd = Proc::new(&inv.exe);
    cmd.args(&inv.args).current_dir(&cwd);
    for k in &inv.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &inv.env {
        cmd.env(k, v);
    }
    let output = match cmd.stdin(Stdio::null()).output() {
        Ok(o) => o,
        Err(e) => {
            return RunOutcome {
                passed: false,
                detail: format!("failed to spawn {}: {e}", inv.exe.display()),
                output: String::new(),
            };
        }
    };

    let mut captured = String::from_utf8_lossy(&output.stdout).into_owned();
    captured.push_str(&String::from_utf8_lossy(&output.stderr));

    let failed = !output.status.success();
    let status_word = describe_status(&output.status);
    // Pass criterion (§3.2): exit 0 => pass; expect-failure passes on
    // nonzero exit OR signal death (shell semantics).
    let (passed, detail) = if inv.expect_failure {
        if failed {
            (true, format!("ok (expected failure: {status_word})"))
        } else {
            (false, "expected failure, but exited 0".to_string())
        }
    } else if failed {
        (false, status_word)
    } else {
        (true, "ok".to_string())
    };
    RunOutcome {
        passed,
        detail,
        output: captured,
    }
}

/// Lexically resolve `.` and `..` components (no filesystem access — the
/// path may not exist yet; symlinks are deliberately not chased). `..` at
/// the root sticks to the root, so an escape attempt can never normalize
/// back inside.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop a real component; keep prefix/root anchors in place.
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir) | Some(Component::Prefix(_))
                ) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn describe_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("killed by signal {sig}");
        }
    }
    match status.code() {
        Some(0) => "exit 0".to_string(),
        Some(c) => format!("exit {c}"),
        None => "terminated".to_string(),
    }
}

fn report_outcome(inv: &Invocation, outcome: &RunOutcome, verbose: bool) {
    eprintln!(
        "cpp-pkg: test {} ... {}",
        inv.label,
        if outcome.passed {
            outcome.detail.clone()
        } else {
            format!("FAILED ({})", outcome.detail)
        }
    );
    if (!outcome.passed || verbose) && !outcome.output.is_empty() {
        eprintln!("---- output of {} ----", inv.label);
        eprint!("{}", outcome.output);
        if !outcome.output.ends_with('\n') {
            eprintln!();
        }
        eprintln!("----");
    }
}

fn run_invocations(
    root: &Path,
    invocations: Vec<Invocation>,
    jobs: usize,
    verbose: bool,
) -> Result<()> {
    let total = invocations.len();
    let outcomes: Vec<RunOutcome> = if jobs <= 1 {
        // Serial default (§3.2): shared fixture cwds make this the safe
        // mode; results print live.
        invocations
            .iter()
            .map(|inv| {
                let o = run_one(root, inv);
                report_outcome(inv, &o, verbose);
                o
            })
            .collect()
    } else {
        // --jobs: simple index-queue worker pool; results print in
        // declaration order once all workers finish.
        use std::sync::Mutex;
        let next = Mutex::new(0usize);
        let results: Vec<Mutex<Option<RunOutcome>>> =
            (0..total).map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..jobs.min(total.max(1)) {
                s.spawn(|| {
                    loop {
                        let i = {
                            let mut n = next.lock().unwrap();
                            if *n >= total {
                                break;
                            }
                            let i = *n;
                            *n += 1;
                            i
                        };
                        let o = run_one(root, &invocations[i]);
                        *results[i].lock().unwrap() = Some(o);
                    }
                });
            }
        });
        let outcomes: Vec<RunOutcome> = results
            .into_iter()
            .map(|m| m.into_inner().unwrap().expect("worker filled every slot"))
            .collect();
        for (inv, o) in invocations.iter().zip(&outcomes) {
            report_outcome(inv, o, verbose);
        }
        outcomes
    };

    let failed = outcomes.iter().filter(|o| !o.passed).count();
    let passed = total - failed;
    let targets: BTreeSet<&str> = invocations.iter().map(|i| i.target.as_str()).collect();
    eprintln!(
        "cpp-pkg: test summary: {passed} passed, {failed} failed \
         ({total} invocation{} across {} test target{})",
        plural(total),
        targets.len(),
        plural(targets.len())
    );
    if failed > 0 {
        bail!("{failed} of {total} test invocation{} failed", plural(total));
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ---------------------------------------------------------------------------
// `install` (spec §6.2)

fn install(
    targets: &[String],
    prefix: &Path,
    destdir_arg: Option<&Path>,
    config_key: &str,
    toolchain_arg: Option<&str>,
    list: bool,
    allow_undeclared: bool,
) -> Result<()> {
    let sess = open_session(config_key, toolchain_arg)?;
    let prefix_str = prefix.to_string_lossy().into_owned();
    let request = BuildRequest::Default;
    let pb = plan_and_write(&sess, &request, Some(&prefix_str), allow_undeclared)?;

    // `--list` is the audit tool (§6.2): plan without building or writing.
    if !list && !pb.plan.targets.is_empty() {
        ninja_gen::run_ninja(&pb.build_dir, &[])?;
    }

    let export_targets: BTreeMap<String, ExportTarget> = pb
        .plan
        .targets
        .iter()
        .map(|t| (t.name.clone(), export_target_of(t)))
        .collect();
    let dev_dep_keys: BTreeSet<String> =
        sess.project.dev_dependencies.keys().cloned().collect();
    let export = ExportInputs {
        package_name: &sess.project.package.name,
        version: sess.project.package.version.as_deref(),
        names: &sess.project.export,
        config: sess.config,
        targets: &export_targets,
        dev_dep_keys: &dev_dep_keys,
    };
    let iplan = shim::plan_install(&InstallRequest {
        project: &sess.project,
        export: &export,
        project_root: &sess.root,
        build_dir: &pb.build_dir,
        prefix,
        lockfile: &pb.deps.lockfile,
        patch_bytes: &pb.deps.patch_bytes,
        sysdeps: &pb.deps.sysdep_decls,
        targets,
    })?;
    for w in &iplan.warnings {
        eprintln!("cpp-pkg: warning: {w}");
    }

    if list {
        print!("{}", iplan.describe());
        return Ok(());
    }
    let destdir_env = std::env::var_os("DESTDIR").map(PathBuf::from);
    let destdir = destdir_arg.or(destdir_env.as_deref());
    shim::execute_install(&iplan, destdir)?;
    eprintln!(
        "cpp-pkg: installed {} file{} under {}{}",
        iplan.actions.len(),
        plural(iplan.actions.len()),
        destdir.map(|d| d.display().to_string()).unwrap_or_default(),
        prefix.display()
    );
    Ok(())
}

/// Copy graph's cfg-projected export metadata into shim's mirror struct
/// (never re-reading raw TOML — the §6.3 contract).
fn export_target_of(t: &PlannedTarget) -> ExportTarget {
    let mut x = ExportTarget::new(t.kind, t.output.clone());
    x.install = t.install;
    x.dev = t.dev;
    x.test = t.test;
    x.public_includes = t.public_includes.clone();
    x.public_defines = t.public_defines.clone();
    x.public_flags = t.public_flags.clone();
    x.public_link_flags = t.public_link_flags.clone();
    x.cxx_std = t.cxx_std;
    x.public_headers = t.public_headers.clone();
    x.runtime_data = t.runtime_data.clone();
    x.local_deps_public = t.local_deps_public.clone();
    x.local_deps_private = t.local_deps_private.clone();
    x.external_public = t.external_public.clone();
    x.external_link_only = t.external_link_only.clone();
    x.external_dep_keys = t.external_deps.keys().cloned().collect();
    x
}

// ---------------------------------------------------------------------------
// `gen` / `gen --check` / hidden `gen-exec` (spec §4.2)

fn gen_verb(check: bool) -> Result<()> {
    let root = std::env::current_dir().context("determining current directory")?;
    let (project, warnings) = schema::load(&root.join("CppPkg.toml"))?;
    print_warnings(&warnings);

    let steps: Vec<&GenerateStep> = project
        .generate
        .values()
        .filter(|s| s.checked_in.is_some())
        .collect();
    if steps.is_empty() {
        eprintln!("cpp-pkg: no checked-in [generate] steps");
        return Ok(());
    }

    let pins = load_pins(&root)?;
    let gen_root = root.join("build").join("gen");
    let build_dir = root.join("build");
    let ictx = InterpCtx {
        package_name: &project.package.name,
        package_version: project.package.version.as_deref(),
        pins: &pins,
        gen_root: Some(&gen_root),
        project_root: Some(&root),
        build_dir: Some(&build_dir),
        install_prefix: None,
    };

    let mut drifted: Vec<String> = Vec::new();
    for step in steps {
        // Checked-in steps validate inputs only here (§4.2): loud exactly
        // when regeneration is asked for, silent on ordinary builds.
        validate_step_inputs(&root, step, &ictx)?;
        let bytes = execute_gen_step(&root, step, &ictx)?;
        let dest_rel = step.checked_in.as_ref().expect("filtered on checked_in");
        let dest = root.join(dest_rel);
        if check {
            let current = fs::read(&dest).unwrap_or_default();
            if current != bytes {
                drifted.push(dest_rel.clone());
                eprintln!("cpp-pkg: gen --check: `{dest_rel}` is out of date (step `{}`)", step.name);
            } else {
                eprintln!("cpp-pkg: gen --check: `{dest_rel}` is current");
            }
        } else {
            // The one sanctioned source-tree write, via this explicit verb.
            let changed = commit_output(&dest, &bytes)?;
            eprintln!(
                "cpp-pkg: gen: `{dest_rel}` {}",
                if changed { "refreshed" } else { "already current" }
            );
        }
    }
    if !drifted.is_empty() {
        bail!(
            "{} checked-in output{} out of date ({}); run `cpp-pkg gen` to refresh",
            drifted.len(),
            plural(drifted.len()),
            drifted.join(", ")
        );
    }
    Ok(())
}

/// Hidden verb ninja invokes for every activated gen edge: re-read the
/// manifest, re-resolve the one step, execute sandboxed, atomic-commit the
/// declared output (mtime preserved on identical bytes — restat = 1).
fn gen_exec(project_root: &Path, step_name: &str) -> Result<()> {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let (project, _warnings) = schema::load(&root.join("CppPkg.toml"))?;
    let step = project.generate.get(step_name).ok_or_else(|| {
        anyhow!(
            "gen-exec: no [generate.{step_name}] step in {} (stale build.ninja? re-run cpp-pkg build)",
            root.join("CppPkg.toml").display()
        )
    })?;

    let pins = load_pins(&root)?;
    let gen_root = root.join("build").join("gen");
    let build_dir = root.join("build");
    let ictx = InterpCtx {
        package_name: &project.package.name,
        package_version: project.package.version.as_deref(),
        pins: &pins,
        gen_root: Some(&gen_root),
        project_root: Some(&root),
        build_dir: Some(&build_dir),
        install_prefix: None,
    };
    let bytes = execute_gen_step(&root, step, &ictx)?;
    let dest = gen_root.join(step.action.output());
    commit_output(&dest, &bytes)?;
    Ok(())
}

/// Pins for interpolation, straight from CppPkg.lock (base commits — never
/// composed patch ids, §5.2).
fn load_pins(root: &Path) -> Result<BTreeMap<String, PinInfo>> {
    let mut pins = BTreeMap::new();
    if let Some(lock) = Lockfile::load(&root.join("CppPkg.lock"))? {
        for (key, row) in &lock.packages {
            if let Some(id) = row.commit.as_ref().or(row.content_hash.as_ref()) {
                pins.insert(
                    key.clone(),
                    PinInfo {
                        commit: id.clone(),
                        requested: requested_human(&row.requested),
                    },
                );
            }
        }
    }
    Ok(pins)
}

/// `${pin.<dep>.requested}` is the human ref: `v1.9.5` for `tag:v1.9.5`,
/// the sha for `rev:<sha>` (interp contract).
fn requested_human(requested: &str) -> String {
    requested
        .strip_prefix("tag:")
        .or_else(|| requested.strip_prefix("rev:"))
        .unwrap_or(requested)
        .to_string()
}

fn interp_step_string(step: &str, raw: &str, pos: InterpPos, ictx: &InterpCtx) -> Result<String> {
    interp::interpolate(raw, pos, ictx)
        .map_err(|e| anyhow!("generate step `{step}`: in `{raw}`: {e}"))
}

/// Resolve a step-declared path: interpolated (absolute ${gen} allowed),
/// then anchored at the project root when relative.
fn step_path(root: &Path, step: &str, raw: &str, ictx: &InterpCtx) -> Result<PathBuf> {
    let s = interp_step_string(step, raw, InterpPos::GenerateArgv, ictx)?;
    let p = PathBuf::from(&s);
    Ok(if p.is_absolute() { p } else { root.join(p) })
}

fn validate_step_inputs(root: &Path, step: &GenerateStep, ictx: &InterpCtx) -> Result<()> {
    for raw in &step.inputs {
        let p = step_path(root, &step.name, raw, ictx)?;
        if !p.is_file() {
            bail!(
                "generate step `{}`: declared input `{raw}` not found (looked at {})",
                step.name,
                p.display()
            );
        }
    }
    Ok(())
}

/// Execute one step and return the output bytes (not yet committed).
fn execute_gen_step(root: &Path, step: &GenerateStep, ictx: &InterpCtx) -> Result<Vec<u8>> {
    match &step.action {
        GenerateAction::Template { template, vars, .. } => {
            let tpath = step_path(root, &step.name, template, ictx)?;
            let mut ivars = BTreeMap::new();
            for (k, v) in vars {
                // Vars carry package/pin identity only — the §0.3 table
                // grants ${gen} to argv, not vars.
                ivars.insert(
                    k.clone(),
                    interp_step_string(&step.name, v, InterpPos::GenerateVar, ictx)?,
                );
            }
            substitute_template(&step.name, &tpath, &ivars)
        }
        GenerateAction::Command { argv, stdin, .. } => {
            let mut iargv = Vec::with_capacity(argv.len());
            for a in argv {
                iargv.push(interp_step_string(&step.name, a, InterpPos::GenerateArgv, ictx)?);
            }
            let stdin_path = match stdin {
                Some(s) => Some(step_path(root, &step.name, s, ictx)?),
                None => None,
            };
            run_gen_command(root, &step.name, &iargv, stdin_path.as_deref())
        }
    }
}

/// Tier-a template substitution: `@VAR@` only (`@ONLY` parity). Unbound
/// token = hard error naming the template line (never CMake's silent
/// empty); unused var = warning; `#cmakedefine` = hard error (§4.2).
fn substitute_template(
    step: &str,
    template: &Path,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let text = fs::read_to_string(template).with_context(|| {
        format!(
            "generate step `{step}`: reading template {}",
            template.display()
        )
    })?;
    if text.contains("#cmakedefine") {
        bail!(
            "generate step `{step}`: template {} uses #cmakedefine, which is \
             not supported in v1 (spell the define out with @VAR@ substitution)",
            template.display()
        );
    }

    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut out = String::with_capacity(text.len());
    for (lineno, line) in text.split_inclusive('\n').enumerate() {
        let mut rest = line;
        while let Some(start) = rest.find('@') {
            let (head, after_at) = rest.split_at(start);
            out.push_str(head);
            let body = &after_at[1..];
            // Token = @[A-Za-z_][A-Za-z0-9_]*@ — anything else (stray '@',
            // email-shaped text) passes through literally.
            let token_len = body
                .char_indices()
                .take_while(|(i, c)| {
                    if *i == 0 {
                        c.is_ascii_alphabetic() || *c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || *c == '_'
                    }
                })
                .count();
            if token_len > 0 && body[token_len..].starts_with('@') {
                let token = &body[..token_len];
                match vars.get(token) {
                    Some(value) => {
                        // vars is keyed by owned Strings; re-borrow the key
                        // so `used` outlives this loop iteration.
                        let (key, _) = vars.get_key_value(token).expect("just found");
                        used.insert(key.as_str());
                        out.push_str(value);
                    }
                    None => bail!(
                        "generate step `{step}`: template {} line {}: unbound \
                         token @{token}@ (declared vars: {})",
                        template.display(),
                        lineno + 1,
                        if vars.is_empty() {
                            "none".to_string()
                        } else {
                            vars.keys().cloned().collect::<Vec<_>>().join(", ")
                        }
                    ),
                }
                rest = &body[token_len + 1..];
            } else {
                out.push('@');
                rest = body;
            }
        }
        out.push_str(rest);
    }

    for k in vars.keys() {
        if !used.contains(k.as_str()) {
            eprintln!(
                "cpp-pkg: warning (generate step {step}): var `{k}` is unused \
                 by template {}",
                template.display()
            );
        }
    }
    Ok(out.into_bytes())
}

/// Run a tier-b command: argv (no shell), cwd = project root, stdout
/// captured as the output, best-effort network sandbox (§4.2: policy
/// normative, enforcement best-effort; failure to *spawn* a sandbox is
/// never a build failure).
fn run_gen_command(
    root: &Path,
    step: &str,
    argv: &[String],
    stdin: Option<&Path>,
) -> Result<Vec<u8>> {
    let spawn = |argv: &[String]| -> std::io::Result<std::process::Output> {
        let mut cmd = Proc::new(&argv[0]);
        cmd.args(&argv[1..]).current_dir(root);
        match stdin {
            Some(p) => {
                cmd.stdin(fs::File::open(p).map_err(|e| {
                    std::io::Error::new(e.kind(), format!("opening stdin {}: {e}", p.display()))
                })?);
            }
            None => {
                cmd.stdin(Stdio::null());
            }
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).output()
    };

    let output = match sandbox_wrapper() {
        Some(wrapper) => {
            let mut wrapped: Vec<String> = wrapper.clone();
            wrapped.extend(argv.iter().cloned());
            match spawn(&wrapped) {
                Ok(o) => o,
                Err(e) => {
                    // Sandbox failed to spawn: degrade loudly, never fail
                    // the build for it (§4.2).
                    eprintln!(
                        "cpp-pkg: warning (generate step {step}): network \
                         sandbox unavailable ({e}); running unsandboxed"
                    );
                    spawn(argv).with_context(|| {
                        format!("generate step `{step}`: spawning `{}`", argv[0])
                    })?
                }
            }
        }
        None => {
            eprintln!(
                "cpp-pkg: warning (generate step {step}): no network sandbox \
                 on this system; running unsandboxed"
            );
            spawn(argv)
                .with_context(|| format!("generate step `{step}`: spawning `{}`", argv[0]))?
        }
    };
    if !output.status.success() {
        bail!(
            "generate step `{step}`: command `{}` failed ({})",
            argv.join(" "),
            describe_status(&output.status)
        );
    }
    Ok(output.stdout)
}

/// Platform sandbox argv prefix, or None when unavailable. Probed once.
fn sandbox_wrapper() -> Option<Vec<String>> {
    use std::sync::OnceLock;
    static WRAPPER: OnceLock<Option<Vec<String>>> = OnceLock::new();
    WRAPPER
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                let sb = Path::new("/usr/bin/sandbox-exec");
                if sb.is_file() {
                    return Some(vec![
                        sb.to_string_lossy().into_owned(),
                        "-p".to_string(),
                        "(version 1)(allow default)(deny network*)".to_string(),
                    ]);
                }
                None
            }
            #[cfg(target_os = "linux")]
            {
                // Unprivileged user namespaces may be disabled (container
                // defaults): probe once, degrade with a warning if so.
                let ok = Proc::new("unshare")
                    .args(["-n", "--", "true"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    return Some(vec![
                        "unshare".to_string(),
                        "-n".to_string(),
                        "--".to_string(),
                    ]);
                }
                None
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                None
            }
        })
        .clone()
}

/// Write `bytes` to `dest` atomically (temp + rename in the same dir).
/// Byte-identical existing content is left untouched — mtime preserved, so
/// ninja's restat = 1 short-circuits downstream rebuilds. Returns whether
/// the file changed.
fn commit_output(dest: &Path, bytes: &[u8]) -> Result<bool> {
    if let Ok(existing) = fs::read(dest)
        && existing == bytes
    {
        return Ok(false);
    }
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("output path {} has no parent", dest.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".cppkg-tmp-{}-{}",
        std::process::id(),
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "out".to_string())
    ));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, dest)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), dest.display()))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// `provide`

fn provide(package: &str, project_dir: &Path, config_key: &str) -> Result<()> {
    // stdout is the wire protocol here (the provider script captures it as
    // the shim path), so everything human-facing goes to stderr.
    let (project, warnings) = schema::load(&project_dir.join("CppPkg.toml"))?;
    print_warnings(&warnings);

    let config = BuildConfig::from_key(config_key)?;
    let toolchain = toolchain::detect_default()?;
    let truth = toolchain.identity.cfg_truth()?;
    let base_profile = project
        .profiles
        .get(config.key())
        .cloned()
        .unwrap_or_default();
    let profile = effective_profile(&project.flags, &truth, &base_profile);
    let abi_flags = profile_abi_flags(&profile);

    let dep_key = project
        .dependencies
        .iter()
        .find(|(key, spec)| spec.find_package.as_deref().unwrap_or(key) == package)
        .map(|(key, _)| key.clone())
        .ok_or_else(|| {
            anyhow!(
                "no [dependencies] entry in {} provides find_package({package}) — add one \
                 (use `find-package = \"{package}\"` when the dependency key differs)",
                project_dir.join("CppPkg.toml").display()
            )
        })?;

    let deps = prepare_dependencies(
        &project,
        project_dir,
        &toolchain,
        config,
        &abi_flags,
        DepScope::Provide { key: &dep_key },
        false,
    )?;

    // The shim is a pure function of the manifest, which is a pure function
    // of (dep, config hash) — so the cache dir is keyed the same way as the
    // artifact entry and can be regenerated at will.
    let shim_dir = deps
        .store_root
        .join("shim")
        .join(format!("{dep_key}-{}", deps.hashes[&dep_key]));
    shim::write_config_shim(&deps.manifests[&dep_key], package, &shim_dir)?;

    // A real <pkg>Config.cmake would find_dependency() its needs; the shim
    // instead loads sibling shims directly (each is idempotent through its
    // if(NOT TARGET) guards), so transitive imported targets exist by the
    // time the consumer's generate step resolves link references.
    let mut includes = String::new();
    for (key, m) in &deps.manifests {
        if key == &dep_key {
            continue;
        }
        let find_name = project.dependencies[key]
            .find_package
            .clone()
            .unwrap_or_else(|| key.clone());
        shim::write_config_shim(m, &find_name, &shim_dir)?;
        includes.push_str(&format!(
            "include(\"${{CMAKE_CURRENT_LIST_DIR}}/{find_name}Config.cmake\")\n"
        ));
    }
    if !includes.is_empty() {
        let config_path = shim_dir.join(format!("{package}Config.cmake"));
        let mut text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        text.push_str("\n# Transitive `needs` packages (find_dependency equivalent).\n");
        text.push_str(&includes);
        std::fs::write(&config_path, text)
            .with_context(|| format!("writing {}", config_path.display()))?;
    }

    println!("{}", shim_dir.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared pipeline: lock + provision every requested dependency.

enum DepScope<'a> {
    /// Ordinary build: provision exactly `required` (graph::required_deps);
    /// eagerly lock the whole declared universe (§3.2).
    Build { required: &'a BTreeSet<String> },
    /// `provide`: the requested dependency plus its `needs` closure, v0
    /// slice semantics (no pruning, no eager locking of unrelated deps).
    Provide { key: &'a str },
}

struct DepArtifacts {
    manifests: BTreeMap<String, Manifest>,
    /// Config hash (fetched deps) or sysdep hash (system deps) per key —
    /// also the artifact/shim cache key for fetched deps.
    hashes: BTreeMap<String, String>,
    /// Install prefix per FETCHED dep (system deps have no artifacts).
    installs: BTreeMap<String, PathBuf>,
    store_root: PathBuf,
    lockfile: Lockfile,
    /// ${pin.*} inputs (base commits, human requested refs).
    pins: BTreeMap<String, PinInfo>,
    /// dep key -> ordered ("blake3:<hex>", bytes) for declared patches.
    patch_bytes: BTreeMap<String, Vec<(String, Vec<u8>)>>,
    /// dep key -> machine-independent system-requirement declaration
    /// (spec §6.3: declarations only, never machine paths).
    sysdep_decls: BTreeMap<String, serde_json::Value>,
}

fn prepare_dependencies(
    project: &ProjectFile,
    root: &Path,
    toolchain: &Toolchain,
    config: BuildConfig,
    abi_flags: &[String],
    scope: DepScope,
    allow_undeclared: bool,
) -> Result<DepArtifacts> {
    let stores = Stores::open_default()?;

    // One namespace across both tables (§3.2; schema rejects collisions).
    let mut combined: BTreeMap<String, DependencySpec> = project.dependencies.clone();
    combined.extend(
        project
            .dev_dependencies
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );

    let mut order = schema::dependency_build_order(&combined)?;
    match &scope {
        DepScope::Build { required } => order.retain(|k| required.contains(k)),
        DepScope::Provide { key } => {
            let mut wanted: BTreeSet<String> = schema::needs_closure(&combined, key)?
                .into_iter()
                .collect();
            wanted.insert((*key).to_string());
            order.retain(|k| wanted.contains(k));
        }
    }

    // Declared patch bytes, read up front: eager lock rows need the hashes,
    // provisioning needs the bytes, install staging needs both.
    let mut patch_bytes: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for (key, spec) in &combined {
        if spec.patches.is_empty() {
            continue;
        }
        patch_bytes.insert(key.clone(), read_patches(root, key, spec)?);
    }

    let mut result = DepArtifacts {
        manifests: BTreeMap::new(),
        hashes: BTreeMap::new(),
        installs: BTreeMap::new(),
        store_root: stores.root.clone(),
        lockfile: Lockfile {
            packages: BTreeMap::new(),
        },
        pins: BTreeMap::new(),
        patch_bytes,
        sysdep_decls: BTreeMap::new(),
    };

    let lock_path = root.join("CppPkg.lock");
    let mut lockfile = Lockfile::load(&lock_path)?.unwrap_or(Lockfile {
        packages: BTreeMap::new(),
    });

    // System-requirement declarations for every declared sysdep (install
    // needs them even when this build didn't provision the dep).
    for (key, spec) in &combined {
        if let SourceSpec::System { min_version } = &spec.source {
            let mut decl = serde_json::Map::new();
            decl.insert("source".into(), serde_json::Value::String("system".into()));
            if let Some(v) = min_version {
                decl.insert("min-version".into(), serde_json::Value::String(v.clone()));
            }
            result
                .sysdep_decls
                .insert(key.clone(), serde_json::Value::Object(decl));
        }
    }

    let mut lock_changed = false;
    if matches!(scope, DepScope::Build { .. }) {
        // Stale entries (deps removed from CppPkg.toml) are pruned on full
        // builds only: `provide` works on a needs-closure slice, so pruning
        // there would discard live pins.
        lock_changed |= prune_lockfile(&mut lockfile, &combined);
        // Eager locking (§3.2): every declared dep gets a row — dev,
        // cfg-inactive, and system alike. Machine facts never enter.
        lock_changed |= eager_lock_rows(&mut lockfile, &combined, &result.patch_bytes)?;
    }
    if lock_changed {
        lockfile.save(&lock_path)?;
        lock_changed = false;
    }

    if order.is_empty() {
        finish_pins(&mut result, lockfile);
        return Ok(result);
    }

    // Coarse whole-build lock: concurrent cpp-pkg processes serialize on the
    // store rather than racing on entries.
    let _lock = stores.lock()?;

    let toolchain_id = toolchain.identity.hash_input();
    // find_package name + machine paths per provisioned sysdep, for the
    // hermetic find gate + leak-scan allowlists.
    let mut sysdep_allow_data: BTreeMap<String, (String, Vec<PathBuf>)> = BTreeMap::new();

    for key in &order {
        let spec = &combined[key];

        if matches!(spec.source, SourceSpec::System { .. }) {
            let (manifest, hash) = provision_sysdep(&stores, key, spec, toolchain)?;
            let mut paths: Vec<PathBuf> = Vec::new();
            if let Ok(facts) = read_sysdep_facts(&stores, key, &hash) {
                paths.extend(facts.library_paths.iter().map(PathBuf::from));
                paths.extend(facts.include_dirs.iter().map(PathBuf::from));
            }
            let find_name = spec.find_package.clone().unwrap_or_else(|| key.clone());
            sysdep_allow_data.insert(key.clone(), (find_name, paths));
            result.manifests.insert(key.clone(), manifest);
            result.hashes.insert(key.clone(), hash);
            continue;
        }

        let source = lockfile::source_string(&spec.source);
        let requested = lockfile::requested_string(&spec.source);
        let locked = lockfile.matching_entry(key, &source, &requested);
        let empty: Vec<(String, Vec<u8>)> = Vec::new();
        let labeled = result.patch_bytes.get(key).unwrap_or(&empty);
        // read_patches reads spec.patches in declaration order, so the two
        // run parallel — the declared path rides along so a hunk failure can
        // name WHICH patch file to re-diff (§5.2).
        let patches: Vec<(PathBuf, Vec<u8>)> = labeled
            .iter()
            .zip(&spec.patches)
            .map(|((_, bytes), declared)| (declared.clone(), bytes.clone()))
            .collect();
        let raw = fetch::ensure(&stores, key, spec, locked, &patches)?;

        let is_git = matches!(spec.source, SourceSpec::Git { .. });
        let entry = LockedPackage {
            source,
            requested,
            // Base identity only (§5.2): the composed patched id lives in
            // the store key, never the lockfile.
            commit: is_git.then(|| raw.base_id.clone()),
            content_hash: (!is_git).then(|| raw.base_id.clone()),
            patches: labeled.iter().map(|(id, _)| id.clone()).collect(),
            min_version: None,
        };
        if lockfile.packages.get(key) != Some(&entry) {
            lockfile.packages.insert(key.clone(), entry);
            lock_changed = true;
        }
        // Persist each resolution as it lands: if a later dependency fails
        // to build, the pins made so far must survive — a tag that moves
        // between runs would otherwise re-resolve to a different commit than
        // the store entries this run already built (fetch trusts the pin).
        if lock_changed {
            lockfile.save(&lock_path)?;
            lock_changed = false;
        }

        // Direct needs' hashes suffice: each already folds in its own needs
        // (Nix-derivation-style transitivity). Topo order guarantees they
        // were computed in an earlier iteration; sysdep hashes enter the
        // same way (§5.3).
        let dep_hashes: BTreeMap<String, String> = spec
            .needs
            .iter()
            .map(|n| (n.clone(), result.hashes[n].clone()))
            .collect();
        let hash = hashing::config_hash(&ConfigHashInputs {
            package_id: &raw.package_id,
            options: &spec.options,
            build_type: config.cmake_name(),
            toolchain: &toolchain_id,
            abi_flags,
            dep_hashes: &dep_hashes,
            subdir: spec.subdir.as_deref(),
        });

        let entry_dir = stores.artifact_dir(key, &hash);
        let install_dir = entry_dir.join("install");
        let find_name = spec.find_package.clone().unwrap_or_else(|| key.clone());

        // CMAKE_PREFIX_PATH needs the transitive closure: a loaded
        // fmtConfig.cmake re-runs its own find_dependency calls. System
        // deps contribute no prefix (the machine provides them).
        let closure: BTreeSet<String> = schema::needs_closure(&combined, key)?
            .into_iter()
            .collect();
        let prefix: Vec<PathBuf> = order
            .iter()
            .filter(|k| closure.contains(*k))
            .filter_map(|k| result.installs.get(k).cloned())
            .collect();

        let manifest_path = stores.manifest_path(&entry_dir);
        // A.8: extraction notes print on fresh derivations (probe/re-probe)
        // only, never replayed on cached-manifest reads.
        let mut fresh_manifest = true;
        let manifest = if stores.entry_complete(&entry_dir) {
            if manifest_path.is_file() {
                fresh_manifest = false;
                Manifest::load(&manifest_path)?
            } else {
                // A.8: complete entry, older extractor's manifest — the
                // artifacts are valid, only the manifest is re-derived
                // (cheap re-probe of the installed prefix).
                eprintln!(
                    "cpp-pkg: refreshing extraction manifest for {key} \
                     (extractor v{})",
                    manifest::EXTRACTOR_VERSION
                );
                reprobe_manifest(
                    key, &find_name, &prefix, &install_dir, &entry_dir, config, toolchain,
                    &manifest_path,
                )?
            }
        } else {
            eprintln!(
                "cpp-pkg: building dependency {key} ({}, {})",
                config.cmake_name(),
                &hash[..12.min(hash.len())]
            );
            let allows = sysdep_allows(&closure, &sysdep_allow_data);
            cmake_build::build_dependency(&DepBuildRequest {
                dep_key: key,
                spec,
                source_dir: &raw.path,
                config_hash: &hash,
                entry_dir: &entry_dir,
                config,
                toolchain,
                abi_flags,
                prefix_path: &prefix,
                subdir: spec.subdir.as_deref(),
                sysdep_allow: &allows,
                allow_undeclared_system_libs: allow_undeclared,
            })?;
            reprobe_manifest(
                key, &find_name, &prefix, &install_dir, &entry_dir, config, toolchain,
                &manifest_path,
            )?;
            stores.mark_complete(&entry_dir)?;
            Manifest::load(&manifest_path)?
        };

        // §5.5 layer 1: every manifest read (fresh or cached) passes the
        // hermeticity scan — an old leaked entry fires on the next build
        // that reads it, not only on fresh extraction.
        check_manifest_hermeticity(
            key,
            &manifest,
            &stores,
            toolchain,
            &sysdep_allow_data,
            allow_undeclared,
        )?;

        if fresh_manifest {
            for note in &manifest.notes {
                eprintln!("cpp-pkg: note ({key}): {note}");
            }
        }

        result.manifests.insert(key.clone(), manifest);
        result.hashes.insert(key.clone(), hash);
        result.installs.insert(key.clone(), install_dir);
    }

    finish_pins(&mut result, lockfile);
    Ok(result)
}

fn finish_pins(result: &mut DepArtifacts, lockfile: Lockfile) {
    for (key, row) in &lockfile.packages {
        if let Some(id) = row.commit.as_ref().or(row.content_hash.as_ref()) {
            result.pins.insert(
                key.clone(),
                PinInfo {
                    commit: id.clone(),
                    requested: requested_human(&row.requested),
                },
            );
        }
    }
    result.lockfile = lockfile;
}

/// SysdepAllow slice for one dep build: the provisioned sysdeps in its
/// `needs` closure.
fn sysdep_allows<'a>(
    closure: &BTreeSet<String>,
    data: &'a BTreeMap<String, (String, Vec<PathBuf>)>,
) -> Vec<SysdepAllow<'a>> {
    data.iter()
        .filter(|(k, _)| closure.contains(*k))
        .map(|(_, (find_name, paths))| SysdepAllow {
            find_name,
            paths,
        })
        .collect()
}

/// Probe the installed prefix of a completed entry and (re)write its
/// extraction manifest.
#[allow(clippy::too_many_arguments)]
fn reprobe_manifest(
    key: &str,
    find_name: &str,
    prefix: &[PathBuf],
    install_dir: &Path,
    entry_dir: &Path,
    config: BuildConfig,
    toolchain: &Toolchain,
    manifest_path: &Path,
) -> Result<Manifest> {
    let mut probe_prefix = prefix.to_vec();
    probe_prefix.push(install_dir.to_path_buf());
    let probe_dir = entry_dir.join("probe-tmp");
    let records = probe::probe_installed(find_name, &probe_prefix, config, toolchain, &probe_dir)?;
    let m = manifest::from_probe(key, find_name, config, &records)?;
    m.save(manifest_path)?;
    // The probe tree is scratch; the manifest is the durable output.
    let _ = std::fs::remove_dir_all(&probe_dir);
    Ok(m)
}

/// §5.5 layer 1 wiring: store-rooted paths are covered by dep_hashes,
/// declared-sysdep paths by the sysdep hash. The SDK sysroot is treated as
/// covered too: `ToolchainIdentity.sdk_version` is already a config-hash
/// input (same reasoning as the cmake_build cache-scan allowance — macOS
/// system libs like zlib legitimately resolve into the SDK).
fn check_manifest_hermeticity(
    key: &str,
    m: &Manifest,
    stores: &Stores,
    toolchain: &Toolchain,
    sysdep_allow_data: &BTreeMap<String, (String, Vec<PathBuf>)>,
    allow_undeclared: bool,
) -> Result<()> {
    let mut allow = HermeticityAllow {
        store_roots: vec![stores.root.clone()],
        sysdep_paths: Vec::new(),
    };
    if let Some(sdk) = &toolchain.sdk_path {
        allow.store_roots.push(sdk.clone());
    }
    for (_, paths) in sysdep_allow_data.values() {
        allow.sysdep_paths.extend(paths.iter().cloned());
    }
    let leaks = manifest::scan_hermeticity(m, &allow);
    if leaks.is_empty() {
        return Ok(());
    }
    let mut msgs = Vec::new();
    for leak in &leaks {
        msgs.push(format!(
            "dependency `{key}`: component `{}` records undeclared absolute \
             path {} — every absolute path in a store manifest must be \
             covered by a hash input (spec §5.5); declare the providing \
             package as `[dependencies.<name>] system = true`, or disable \
             the feature that pulls it in",
            leak.component,
            leak.path.display()
        ));
    }
    if allow_undeclared {
        for m in msgs {
            eprintln!("cpp-pkg: warning: {m}");
        }
        Ok(())
    } else {
        bail!(
            "{}\n(cpp-pkg build --allow-undeclared-system-libs downgrades \
             this to a warning; unsupported for sharing)",
            msgs.join("\n")
        )
    }
}

/// Read a dependency's declared patch files (manifest-dir-relative) into
/// ("blake3:<hex>", bytes) rows, declaration order (§5.2).
fn read_patches(
    root: &Path,
    key: &str,
    spec: &DependencySpec,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::with_capacity(spec.patches.len());
    for rel in &spec.patches {
        let path = if rel.is_absolute() {
            rel.clone()
        } else {
            root.join(rel)
        };
        let bytes = fs::read(&path).with_context(|| {
            format!(
                "dependency `{key}`: patch file `{}` not readable (looked at {})",
                rel.display(),
                path.display()
            )
        })?;
        out.push((hashing::blake3_bytes_labeled(&bytes), bytes));
    }
    Ok(out)
}

/// §3.2 eager locking: give every declared dependency a row. System deps
/// lock as pure declarations; git deps resolve their ref (network only for
/// unpinned tags); url deps lock on first provisioning (their declared
/// sha256 already pins content machine-independently — recorded deviation).
fn eager_lock_rows(
    lockfile: &mut Lockfile,
    combined: &BTreeMap<String, DependencySpec>,
    patch_bytes: &BTreeMap<String, Vec<(String, Vec<u8>)>>,
) -> Result<bool> {
    let mut changed = false;
    for (key, spec) in combined {
        let source = lockfile::source_string(&spec.source);
        let requested = lockfile::requested_string(&spec.source);
        let patch_ids: Vec<String> = patch_bytes
            .get(key)
            .map(|v| v.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();

        let desired = match &spec.source {
            SourceSpec::System { min_version } => LockedPackage {
                source,
                requested,
                commit: None,
                content_hash: None,
                patches: Vec::new(),
                min_version: min_version.clone(),
            },
            SourceSpec::Git { url, reference } => {
                let commit = match lockfile.matching_entry(key, &source, &requested) {
                    Some(row) if row.commit.is_some() => row.commit.clone().unwrap(),
                    _ => match reference {
                        GitRef::Rev(rev) => fetch::validated_commit(key, "rev", rev)?,
                        GitRef::Tag(tag) => {
                            eprintln!("cpp-pkg: resolving {key} tag {tag} (eager lock)");
                            fetch::resolve_git_tag(url, tag)?
                        }
                    },
                };
                LockedPackage {
                    source,
                    requested,
                    commit: Some(commit),
                    content_hash: None,
                    patches: patch_ids,
                    min_version: None,
                }
            }
            SourceSpec::Url { .. } => {
                // Content hash needs the bytes; do not download for a dep
                // this build never provisions. The row appears on first
                // provisioning.
                match lockfile.matching_entry(key, &source, &requested) {
                    Some(row) => LockedPackage {
                        patches: patch_ids,
                        ..row.clone()
                    },
                    None => continue,
                }
            }
        };
        if lockfile.packages.get(key) != Some(&desired) {
            lockfile.packages.insert(key.clone(), desired);
            changed = true;
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// System dependency provisioning (§5.3)

/// Provision a `system = true` dep: reuse a still-valid sysdep store entry
/// (recorded library hashes re-checked against disk) or probe the machine
/// afresh. Returns the interface manifest and the sysdep hash that enters
/// dependents' dep_hashes.
fn provision_sysdep(
    stores: &Stores,
    key: &str,
    spec: &DependencySpec,
    toolchain: &Toolchain,
) -> Result<(Manifest, String)> {
    let find_name = spec.find_package.clone().unwrap_or_else(|| key.to_string());
    let min_version = match &spec.source {
        SourceSpec::System { min_version } => min_version.as_deref(),
        _ => unreachable!("provision_sysdep called for a non-system dep"),
    };

    // Re-validate existing entries: recorded machine facts must still match
    // the machine (spec §5.3 — re-resolve when file hashes drift).
    let sysdeps_root = stores.root.join("sysdeps");
    if let Ok(entries) = fs::read_dir(&sysdeps_root) {
        for e in entries.flatten() {
            let dir = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&format!("{key}-")) || !stores.entry_complete(&dir) {
                continue;
            }
            let Ok(facts) = read_facts_file(&dir) else {
                continue;
            };
            if !facts_still_valid(&facts) {
                eprintln!(
                    "cpp-pkg: system dependency {key} changed on this machine \
                     (recorded library hashes no longer match — OS update?); \
                     re-probing"
                );
                continue;
            }
            let hash = facts_hash(key, &facts);
            if stores.sysdep_dir(key, &hash) != dir {
                continue; // stale entry written by an older encoder
            }
            let manifest = Manifest::load(&stores.manifest_path(&dir))?;
            return Ok((manifest, hash));
        }
    }

    eprintln!("cpp-pkg: probing system dependency {key} (find_package({find_name}))");
    let work_dir = sysdeps_root.join(format!("probe-tmp-{key}"));
    let probed = probe::probe_system(key, &find_name, min_version, toolchain, &work_dir);
    let _ = fs::remove_dir_all(&work_dir);
    let (manifest, facts) = probed?;

    let hash = facts_hash(key, &facts);
    let entry_dir = stores.sysdep_dir(key, &hash);
    fs::create_dir_all(&entry_dir)
        .with_context(|| format!("creating sysdep entry {}", entry_dir.display()))?;
    manifest.save(&stores.manifest_path(&entry_dir))?;
    let facts_json = serde_json::to_string_pretty(&facts).context("serializing sysdep facts")?;
    fs::write(entry_dir.join("facts.json"), facts_json)
        .with_context(|| format!("writing {}/facts.json", entry_dir.display()))?;
    stores.mark_complete(&entry_dir)?;
    eprintln!(
        "cpp-pkg: system dependency {key}: {} {} ({})",
        find_name,
        if facts.resolved_version.is_empty() {
            "(unversioned)"
        } else {
            &facts.resolved_version
        },
        &hash[..8.min(hash.len())]
    );
    Ok((manifest, hash))
}

fn read_facts_file(entry_dir: &Path) -> Result<SysdepFacts> {
    let path = entry_dir.join("facts.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn read_sysdep_facts(stores: &Stores, key: &str, hash: &str) -> Result<SysdepFacts> {
    read_facts_file(&stores.sysdep_dir(key, hash))
}

/// Are the recorded machine facts still true on this machine?
fn facts_still_valid(facts: &SysdepFacts) -> bool {
    if facts.library_paths.len() != facts.library_hashes.len() {
        return false;
    }
    for (path, recorded) in facts.library_paths.iter().zip(&facts.library_hashes) {
        match fs::read(path) {
            Ok(bytes) => {
                if hashing::blake3_bytes_labeled(&bytes) != *recorded {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    facts.include_dirs.iter().all(|d| Path::new(d).is_dir())
}

fn facts_hash(key: &str, facts: &SysdepFacts) -> String {
    hashing::sysdep_hash(&SysdepHashInputs {
        key,
        resolution_mode: "cmake",
        resolved_version: &facts.resolved_version,
        library_paths: &facts.library_paths,
        library_hashes: &facts.library_hashes,
        include_dirs: &facts.include_dirs,
    })
}

/// Drop lockfile entries whose dependency key no longer exists in either
/// dependency table; returns true if anything was removed. Left in place, a
/// stale entry would silently re-pin a years-old commit if the dependency
/// were ever re-added under the same key.
fn prune_lockfile(
    lockfile: &mut Lockfile,
    combined: &BTreeMap<String, DependencySpec>,
) -> bool {
    let before = lockfile.packages.len();
    lockfile.packages.retain(|key, _| combined.contains_key(key));
    lockfile.packages.len() != before
}

// ---------------------------------------------------------------------------
// Helpers

fn print_warnings(warnings: &Warnings) {
    for w in &warnings.0 {
        eprintln!("cpp-pkg: warning: {w}");
    }
}

/// ABI-classified profile flags, in profile order (cxx-flags, then c-flags,
/// then link-flags), deduped keep-first. These reach dependency builds via
/// the generated toolchain file and fold into every dep's config hash.
/// (With wave 1, the "profile" here is the effective profile — `[flags]`
/// ABI words already injected at its head, see `effective_profile`.)
fn profile_abi_flags(profile: &Profile) -> Vec<String> {
    let mut abi: Vec<String> = Vec::new();
    for list in [&profile.cxx_flags, &profile.c_flags, &profile.link_flags] {
        for flag in &toolchain::classify_flags(list).abi {
            if !abi.contains(flag) {
                abi.push(flag.clone());
            }
        }
    }
    abi
}

/// --toolchain resolution: a [toolchains] preset name wins over a compiler
/// path/command; no argument means `c++` on PATH.
fn select_toolchain(project: &ProjectFile, arg: Option<&str>) -> Result<Toolchain> {
    match arg {
        None => toolchain::detect_default(),
        Some(name) => match project.toolchains.get(name) {
            Some(preset) => {
                let mut tc = toolchain::detect(&preset.cxx)?;
                // Preset cc/ar override the derived tools; identity still
                // comes from detection output, never from the preset name.
                if let Some(cc) = &preset.cc {
                    tc.cc = resolve_tool(cc)?;
                }
                if let Some(ar) = &preset.ar {
                    tc.ar = resolve_tool(ar)?;
                }
                Ok(tc)
            }
            None => toolchain::detect(name),
        },
    }
}

fn resolve_tool(name: &str) -> Result<PathBuf> {
    let p = Path::new(name);
    if name.contains('/') {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        bail!("tool `{name}` not found (not a file)");
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("tool `{name}` not found on PATH");
}

/// Minimal POSIX-shell quoting for human-facing --query output.
fn shell_word(w: &str) -> String {
    let safe = !w.is_empty()
        && w.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&b));
    if safe {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_build_defaults() {
        let cli = Cli::try_parse_from(["cpp-pkg", "build"]).unwrap();
        match cli.command {
            Cmd::Build {
                targets,
                config,
                toolchain,
                prefix,
                allow_undeclared_system_libs,
                query,
            } => {
                assert!(targets.is_empty());
                assert_eq!(config, "release");
                assert!(toolchain.is_none());
                assert!(prefix.is_none());
                assert!(!allow_undeclared_system_libs);
                assert!(query.is_none());
            }
            _ => panic!("expected build"),
        }
    }

    #[test]
    fn cli_parses_query_with_and_without_path() {
        let cli = Cli::try_parse_from(["cpp-pkg", "build", "--query"]).unwrap();
        match cli.command {
            Cmd::Build { query, .. } => assert_eq!(query.as_deref(), Some("")),
            _ => panic!("expected build"),
        }
        let cli = Cli::try_parse_from(["cpp-pkg", "build", "--query", "src/main.cpp"]).unwrap();
        match cli.command {
            Cmd::Build { query, .. } => assert_eq!(query.as_deref(), Some("src/main.cpp")),
            _ => panic!("expected build"),
        }
    }

    #[test]
    fn cli_parses_test_with_passthrough() {
        let cli = Cli::try_parse_from([
            "cpp-pkg", "test", "tzdb", "--jobs", "4", "--", "--gtest_filter=*Load*",
        ])
        .unwrap();
        match cli.command {
            Cmd::Test {
                filters,
                jobs,
                passthrough,
                list,
                verbose,
                ..
            } => {
                assert_eq!(filters, vec!["tzdb".to_string()]);
                assert_eq!(jobs, 4);
                assert_eq!(passthrough, vec!["--gtest_filter=*Load*".to_string()]);
                assert!(!list);
                assert!(!verbose);
            }
            _ => panic!("expected test"),
        }
    }

    #[test]
    fn cli_parses_install() {
        let cli = Cli::try_parse_from([
            "cpp-pkg", "install", "--prefix", "/opt/x", "--destdir", "/tmp/stage", "--list",
        ])
        .unwrap();
        match cli.command {
            Cmd::Install {
                prefix,
                destdir,
                list,
                targets,
                ..
            } => {
                assert_eq!(prefix, PathBuf::from("/opt/x"));
                assert_eq!(destdir, Some(PathBuf::from("/tmp/stage")));
                assert!(list);
                assert!(targets.is_empty());
            }
            _ => panic!("expected install"),
        }
    }

    #[test]
    fn cli_parses_gen_and_gen_exec() {
        let cli = Cli::try_parse_from(["cpp-pkg", "gen", "--check"]).unwrap();
        assert!(matches!(cli.command, Cmd::Gen { check: true }));
        let cli = Cli::try_parse_from([
            "cpp-pkg",
            "gen-exec",
            "--project-root",
            "/tmp/p",
            "--step",
            "version-header",
            "--digest",
            "abcd",
        ])
        .unwrap();
        match cli.command {
            Cmd::GenExec {
                project_root,
                step,
                digest,
            } => {
                assert_eq!(project_root, PathBuf::from("/tmp/p"));
                assert_eq!(step, "version-header");
                assert_eq!(digest.as_deref(), Some("abcd"));
            }
            _ => panic!("expected gen-exec"),
        }
    }

    #[test]
    fn cli_parses_provide() {
        let cli = Cli::try_parse_from([
            "cpp-pkg", "provide", "--package", "fmt", "--project", "/tmp/x",
        ])
        .unwrap();
        match cli.command {
            Cmd::Provide {
                package,
                project,
                config,
            } => {
                assert_eq!(package, "fmt");
                assert_eq!(project, PathBuf::from("/tmp/x"));
                assert_eq!(config, "release");
            }
            _ => panic!("expected provide"),
        }
    }

    #[test]
    fn cli_shell_word_quoting() {
        assert_eq!(shell_word("simple/path.cpp"), "simple/path.cpp");
        assert_eq!(shell_word("has space"), "'has space'");
        assert_eq!(shell_word("it's"), r#"'it'\''s'"#);
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn cli_prune_lockfile_drops_removed_deps() {
        use crate::schema::{GitRef, SourceSpec};
        let entry = |name: &str| LockedPackage {
            source: format!("git+https://example.invalid/{name}"),
            requested: "tag:v1".to_string(),
            commit: Some("a".repeat(40)),
            content_hash: None,
            patches: Vec::new(),
            min_version: None,
        };
        let mut lockfile = Lockfile {
            packages: BTreeMap::from([
                ("fmt".to_string(), entry("fmt")),
                ("zlib".to_string(), entry("zlib")),
            ]),
        };
        let deps = BTreeMap::from([(
            "fmt".to_string(),
            DependencySpec::from_source(SourceSpec::Git {
                url: "https://example.invalid/fmt".to_string(),
                reference: GitRef::Tag("v1".to_string()),
            }),
        )]);
        assert!(prune_lockfile(&mut lockfile, &deps));
        assert!(lockfile.packages.contains_key("fmt"));
        assert!(!lockfile.packages.contains_key("zlib"));
        // Second run: nothing left to prune.
        assert!(!prune_lockfile(&mut lockfile, &deps));
    }

    #[test]
    fn cli_profile_abi_flags_dedup_in_order() {
        let profile = Profile {
            cxx_flags: vec!["-stdlib=libc++".into(), "-O2".into()],
            c_flags: vec!["-D_GLIBCXX_ASSERTIONS".into()],
            link_flags: vec!["-stdlib=libc++".into(), "-fsanitize=address".into()],
        };
        assert_eq!(
            profile_abi_flags(&profile),
            vec!["-stdlib=libc++".to_string(), "-D_GLIBCXX_ASSERTIONS".into()]
        );
    }

    #[test]
    fn cli_effective_profile_injects_package_abi_words() {
        use crate::schema::{CfgAtom, CfgPredicate, PackageFlagsGroup};
        let flags = PackageFlags {
            cxx_flags: vec!["-Wno-deprecated".into(), "-D_GLIBCXX_ASSERTIONS".into()],
            c_flags: vec![],
            link_flags: vec![],
            cfg: vec![(
                CfgPredicate {
                    atom: CfgAtom::Linux,
                },
                PackageFlagsGroup {
                    cxx_flags: vec!["-stdlib=libc++".into()],
                    c_flags: vec![],
                    link_flags: vec![],
                },
            )],
        };
        let profile = Profile {
            cxx_flags: vec!["-O2".into()],
            ..Default::default()
        };
        // Linux truth: the cfg group's ABI word joins the injection set.
        let linux = CfgTruth {
            os: CfgAtom::Linux,
            compiler: CfgAtom::Clang,
        };
        let eff = effective_profile(&flags, &linux, &profile);
        assert_eq!(
            eff.cxx_flags,
            vec![
                "-D_GLIBCXX_ASSERTIONS".to_string(),
                "-stdlib=libc++".into(),
                "-O2".into()
            ]
        );
        // macOS truth: only the unconditional ABI word.
        let mac = CfgTruth {
            os: CfgAtom::Macos,
            compiler: CfgAtom::Clang,
        };
        let eff = effective_profile(&flags, &mac, &profile);
        assert_eq!(
            eff.cxx_flags,
            vec!["-D_GLIBCXX_ASSERTIONS".to_string(), "-O2".into()]
        );
        // Empty [flags]: byte-identical to the bare profile (v0 compat).
        let eff = effective_profile(&PackageFlags::default(), &mac, &profile);
        assert_eq!(eff.cxx_flags, profile.cxx_flags);
        assert!(eff.c_flags.is_empty() && eff.link_flags.is_empty());
    }

    #[test]
    fn cli_requested_human_strips_ref_prefixes() {
        assert_eq!(requested_human("tag:v1.9.5"), "v1.9.5");
        assert_eq!(requested_human("rev:abc123"), "abc123");
        assert_eq!(requested_human("sha256:deadbeef"), "sha256:deadbeef");
        assert_eq!(requested_human("system"), "system");
    }

    #[test]
    fn cli_substitute_template_rules() {
        let dir = std::env::temp_dir().join(format!("cppkg-cli-tpl-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let tpl = dir.join("v.hpp.in");
        fs::write(&tpl, "#define V \"@VERSION@\"\n// a@b no token\n").unwrap();
        let vars = BTreeMap::from([("VERSION".to_string(), "1.4.0".to_string())]);
        let out = substitute_template("s", &tpl, &vars).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#define V \"1.4.0\"\n// a@b no token\n"
        );

        // Unbound token: hard error naming the line.
        fs::write(&tpl, "line one\n@NOPE@\n").unwrap();
        let err = substitute_template("s", &tpl, &vars).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("@NOPE@"), "{err}");

        // #cmakedefine: not supported in v1.
        fs::write(&tpl, "#cmakedefine HAVE_X\n").unwrap();
        let err = substitute_template("s", &tpl, &vars).unwrap_err().to_string();
        assert!(err.contains("not supported in v1"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_commit_output_preserves_identical_bytes() {
        let dir = std::env::temp_dir().join(format!("cppkg-cli-commit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("nested").join("out.h");
        assert!(commit_output(&dest, b"one").unwrap());
        let mtime = fs::metadata(&dest).unwrap().modified().unwrap();
        // Identical bytes: untouched (restat-friendly).
        assert!(!commit_output(&dest, b"one").unwrap());
        assert_eq!(fs::metadata(&dest).unwrap().modified().unwrap(), mtime);
        // Changed bytes: rewritten.
        assert!(commit_output(&dest, b"two").unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"two");
        let _ = fs::remove_dir_all(&dir);
    }
}
