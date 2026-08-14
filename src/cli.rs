//! CLI + build orchestration (per CPP_PKG.md CLI section).
//!
//! v0 surface:
//!   cpp-pkg build [TARGETS...] [--config <debug|release|relwithdebinfo|
//!     minsizerel>] [--toolchain <name-or-path>] [--query [PATH]]
//!   cpp-pkg provide --package <find-name> --project <dir> ... (internal;
//!     used by the dependency provider shim)
//!
//! Orchestration pipeline for `build` (each step's module owns its logic —
//! this function only sequences and reports):
//!   1. schema::load(CppPkg.toml) -> print Warnings
//!   2. toolchain: --toolchain path/preset or detect_default()
//!   3. lockfile::load; stores::open_default + lock
//!   4. schema::dependency_build_order; for each dep:
//!      fetch::ensure (with lock entry) -> update lockfile entry;
//!      hashing::config_hash (needs' hashes from earlier iterations);
//!      if !store.entry_complete: cmake_build::build_dependency,
//!      probe::probe_installed, manifest::from_probe + save,
//!      store.mark_complete; else: manifest::load
//!   5. lockfile::save (only if changed)
//!   6. graph::plan -> ninja_gen::write_ninja + write_compile_commands
//!   7. --query: print compile commands (all targets or the given TU) and
//!      exit without building; otherwise ninja_gen::run_ninja
//!
//! `provide` reuses steps 1–5 restricted to the requested package + its
//! `needs` closure, emits a Config.cmake shim from the manifest, and prints
//! the shim directory as its ONLY stdout output (the provider script captures
//! stdout as the path; all diagnostics go to stderr).
//!
//! `--path <file> --with <dep>...` fast-prototyping flow: DEFERRED post-v0
//! (schema-adjacent CLI recorded in DESIGN_CHOICES.md Open).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::Result;
use crate::cmake_build::{self, DepBuildRequest};
use crate::graph::BuildPlan;
use crate::hashing::{self, ConfigHashInputs};
use crate::lockfile::{self, LockedPackage, Lockfile};
use crate::manifest::{self, Manifest};
use crate::schema::{self, BuildConfig, Profile, ProjectFile, SourceSpec, Warnings};
use crate::store::Stores;
use crate::toolchain::{self, GnuDriver, Toolchain};
use crate::{fetch, graph, ninja_gen, probe, shim};

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
        /// Targets to build (default: all targets).
        targets: Vec<String>,
        /// Build configuration: debug, release, relwithdebinfo, minsizerel.
        #[arg(long, default_value = "release")]
        config: String,
        /// A [toolchains] preset name, or a C++ compiler path/command.
        #[arg(long)]
        toolchain: Option<String>,
        /// Print compile commands instead of building; with PATH, only the
        /// command for that source file.
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
        query: Option<String>,
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
            query,
        } => build(&targets, &config, toolchain.as_deref(), query.as_deref()),
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
// `build`

fn build(
    targets: &[String],
    config_key: &str,
    toolchain_arg: Option<&str>,
    query: Option<&str>,
) -> Result<()> {
    let root = std::env::current_dir().context("determining current directory")?;
    let (project, warnings) = schema::load(&root.join("CppPkg.toml"))?;
    print_warnings(&warnings);

    let config = BuildConfig::from_key(config_key)?;
    let toolchain = select_toolchain(&project, toolchain_arg)?;
    let profile = project
        .profiles
        .get(config.key())
        .cloned()
        .unwrap_or_default();
    let abi_flags = profile_abi_flags(&profile);

    let deps = prepare_dependencies(&project, &root, &toolchain, config, &abi_flags, None)?;

    let plan = graph::plan(&project, &root, &deps.manifests, config, &profile, targets)?;
    if plan.targets.is_empty() {
        eprintln!("cpp-pkg: no targets to build (dependencies are up to date)");
        return Ok(());
    }

    let build_dir = root.join("build");
    let driver = GnuDriver;
    ninja_gen::write_ninja(&plan, &toolchain, &driver, config, &build_dir)?;
    ninja_gen::write_compile_commands(&plan, &toolchain, &driver, config, &build_dir)?;

    if let Some(filter) = query {
        return print_query(&plan, &toolchain, &driver, config, &root, filter);
    }
    ninja_gen::run_ninja(&build_dir, targets)
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
// `provide`

fn provide(package: &str, project_dir: &Path, config_key: &str) -> Result<()> {
    // stdout is the wire protocol here (the provider script captures it as
    // the shim path), so everything human-facing goes to stderr.
    let (project, warnings) = schema::load(&project_dir.join("CppPkg.toml"))?;
    print_warnings(&warnings);

    let config = BuildConfig::from_key(config_key)?;
    let toolchain = toolchain::detect_default()?;
    let profile = project
        .profiles
        .get(config.key())
        .cloned()
        .unwrap_or_default();
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
        Some(&dep_key),
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
// Shared pipeline: fetch + build + probe every requested dependency.

struct DepArtifacts {
    manifests: BTreeMap<String, Manifest>,
    /// Config hash per dep (also the artifact/shim cache key).
    hashes: BTreeMap<String, String>,
    /// Install prefix per dep.
    installs: BTreeMap<String, PathBuf>,
    store_root: PathBuf,
}

/// Steps 3–5 of the pipeline for all dependencies (or, with `only_dep`, that
/// dependency plus its transitive `needs` closure), in `needs` topo order.
fn prepare_dependencies(
    project: &ProjectFile,
    root: &Path,
    toolchain: &Toolchain,
    config: BuildConfig,
    abi_flags: &[String],
    only_dep: Option<&str>,
) -> Result<DepArtifacts> {
    let stores = Stores::open_default()?;
    let mut order = schema::dependency_build_order(&project.dependencies)?;
    if let Some(key) = only_dep {
        let mut wanted: BTreeSet<String> = schema::needs_closure(&project.dependencies, key)?
            .into_iter()
            .collect();
        wanted.insert(key.to_string());
        order.retain(|k| wanted.contains(k));
    }

    let mut result = DepArtifacts {
        manifests: BTreeMap::new(),
        hashes: BTreeMap::new(),
        installs: BTreeMap::new(),
        store_root: stores.root.clone(),
    };

    let lock_path = root.join("CppPkg.lock");
    let mut lockfile = Lockfile::load(&lock_path)?.unwrap_or(Lockfile {
        packages: BTreeMap::new(),
    });
    // Stale entries (deps removed from CppPkg.toml) are pruned on full
    // builds only: `provide` works on a needs-closure slice of
    // [dependencies], so pruning there would discard live pins.
    let mut lock_changed =
        only_dep.is_none() && prune_lockfile(&mut lockfile, &project.dependencies);
    if lock_changed {
        lockfile.save(&lock_path)?;
        lock_changed = false;
    }
    if order.is_empty() {
        return Ok(result);
    }

    // Coarse whole-build lock: concurrent cpp-pkg processes serialize on the
    // store rather than racing on entries.
    let _lock = stores.lock()?;

    let toolchain_id = toolchain.identity.hash_input();

    for key in &order {
        let spec = &project.dependencies[key];
        let source = lockfile::source_string(&spec.source);
        let requested = lockfile::requested_string(&spec.source);
        let locked = lockfile.matching_entry(key, &source, &requested);
        let raw = fetch::ensure(&stores, key, spec, locked)?;

        let is_git = matches!(spec.source, SourceSpec::Git { .. });
        let entry = LockedPackage {
            source,
            requested,
            commit: is_git.then(|| raw.package_id.clone()),
            content_hash: (!is_git).then(|| raw.package_id.clone()),
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
        // were computed in an earlier iteration.
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
        });

        let entry_dir = stores.artifact_dir(key, &hash);
        let install_dir = entry_dir.join("install");
        let find_name = spec.find_package.clone().unwrap_or_else(|| key.clone());

        // CMAKE_PREFIX_PATH needs the transitive closure: a loaded
        // fmtConfig.cmake re-runs its own find_dependency calls.
        let closure: BTreeSet<String> = schema::needs_closure(&project.dependencies, key)?
            .into_iter()
            .collect();
        let prefix: Vec<PathBuf> = order
            .iter()
            .filter(|k| closure.contains(*k))
            .map(|k| result.installs[k].clone())
            .collect();

        let manifest_path = entry_dir.join("cppkg-manifest.json");
        let manifest = if stores.entry_complete(&entry_dir) {
            Manifest::load(&manifest_path)?
        } else {
            eprintln!(
                "cpp-pkg: building dependency {key} ({}, {})",
                config.cmake_name(),
                &hash[..12.min(hash.len())]
            );
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
            })?;

            let mut probe_prefix = prefix.clone();
            probe_prefix.push(install_dir.clone());
            let probe_dir = entry_dir.join("probe-tmp");
            let records =
                probe::probe_installed(&find_name, &probe_prefix, config, toolchain, &probe_dir)?;
            let m = manifest::from_probe(key, &find_name, config, &records)?;
            m.save(&manifest_path)?;
            // The probe tree is scratch; the manifest is the durable output.
            let _ = std::fs::remove_dir_all(&probe_dir);
            stores.mark_complete(&entry_dir)?;
            m
        };

        for note in &manifest.notes {
            eprintln!("cpp-pkg: note ({key}): {note}");
        }

        result.manifests.insert(key.clone(), manifest);
        result.hashes.insert(key.clone(), hash);
        result.installs.insert(key.clone(), install_dir);
    }

    Ok(result)
}

/// Drop lockfile entries whose dependency key no longer exists in
/// CppPkg.toml; returns true if anything was removed. Left in place, a stale
/// entry would silently re-pin a years-old commit if the dependency were
/// ever re-added under the same key.
fn prune_lockfile(
    lockfile: &mut Lockfile,
    deps: &BTreeMap<String, schema::DependencySpec>,
) -> bool {
    let before = lockfile.packages.len();
    lockfile.packages.retain(|key, _| deps.contains_key(key));
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
                query,
            } => {
                assert!(targets.is_empty());
                assert_eq!(config, "release");
                assert!(toolchain.is_none());
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
        use crate::schema::{DependencySpec, ExposesTargets, GitRef, SourceSpec};
        let entry = |name: &str| LockedPackage {
            source: format!("git+https://example.invalid/{name}"),
            requested: "tag:v1".to_string(),
            commit: Some("a".repeat(40)),
            content_hash: None,
        };
        let mut lockfile = Lockfile {
            packages: BTreeMap::from([
                ("fmt".to_string(), entry("fmt")),
                ("zlib".to_string(), entry("zlib")),
            ]),
        };
        let deps = BTreeMap::from([(
            "fmt".to_string(),
            DependencySpec {
                source: SourceSpec::Git {
                    url: "https://example.invalid/fmt".to_string(),
                    reference: GitRef::Tag("v1".to_string()),
                },
                options: BTreeMap::new(),
                needs: vec![],
                find_package: None,
                exposes_namespace: vec![],
                exposes_targets: ExposesTargets::default(),
            },
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
}
