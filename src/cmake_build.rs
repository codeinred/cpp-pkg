//! Dependency builds via CMake (CPP_PKG_IMPLEMENTATION.md §3, §5).
//!
//! Contract:
//! - Generate a toolchain file from the detected toolchain (single source of
//!   truth): sets CMAKE_C(XX)_COMPILER, CMAKE_OSX_SYSROOT (if SDK), CMAKE_AR,
//!   and appends ABI-classified profile flags to CMAKE_C_FLAGS_INIT /
//!   CMAKE_CXX_FLAGS_INIT. Deps never pick a compiler from the environment.
//! - Configure with: -G Ninja, CMAKE_BUILD_TYPE=<config>,
//!   CMAKE_TOOLCHAIN_FILE, CMAKE_INSTALL_PREFIX=<artifact entry>/install,
//!   BUILD_SHARED_LIBS=OFF (overridable by dep options), the dep's literal
//!   `options`, CMAKE_POLICY_VERSION_MINIMUM=3.5 (CMake 4.x dropped <3.5
//!   compat and many real deps still declare old minimums),
//!   CMAKE_PREFIX_PATH=<install dirs of the TRANSITIVE needs closure>.
//! - Scrub the environment: pass a minimal env (PATH, HOME, and the vars
//!   CMake/compilers genuinely need); drop CC/CXX/CFLAGS/CXXFLAGS/
//!   LDFLAGS/CMAKE_* so host config can't leak into hashed builds.
//! - Build + install (cmake --build . && cmake --install .); on success the
//!   caller writes the manifest and marks the store entry complete.
//! - ERROR TRANSLATION (§5): scan the configure log for
//!   `find_dependency`/`find_package` failures and translate BOTH shapes:
//!   not-found -> "add <pkg> to [dependencies] and to <dep>.needs";
//!   version-rejection -> name the pinned version vs the requirement.
//!   Preserve the raw CMake log path in the error for debugging.
//!
//! Wave-1 extensions (spec wave1-extensions.md A.5/A.9, §5.3/§5.5):
//! - `subdir` (A.5): the configure root is `<checkout>/<subdir>` when the dep
//!   declares one (patches were already applied at the checkout root by
//!   fetch, before this module runs).
//! - System dependencies (§5.3): `DepBuildRequest.sysdep_allow` names the
//!   declared `system = true` deps whose find_package results the leak scan
//!   must let through (by find name and by recorded machine paths).
//! - Extended leak scan (§5.5 layer 2): `scan_find_package_leaks` also
//!   polices `*_LIBRARY`/`*_INCLUDE_DIR` cache entries (the find_library
//!   route), returning leak messages so the caller picks error-vs-warn
//!   (`--allow-undeclared-system-libs` downgrades to warnings).
//! - CMake ≥ 4 policy refusal (A.9) is translated into the
//!   `CMAKE_POLICY_VERSION_MINIMUM = "3.5"` options hint.
//! - Unknown dep `options` keys (A.10) are linted after configure by
//!   checking for UNINITIALIZED cache entries (warning only).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};

use crate::schema::{BuildConfig, DependencySpec};
use crate::toolchain::Toolchain;
use crate::Result;

#[derive(Debug, Clone)]
pub struct BuiltDep {
    pub dep_key: String,
    pub config_hash: String,
    /// <artifact entry>/install — the prefix find_package/probe consumes.
    pub install_dir: PathBuf,
}

/// §5.3/§5.5 (wave 1): one dependency key declared `system = true`, expressed
/// as what the leak gate needs to know — the find_package name it is allowed
/// to resolve, and the machine paths its sysdep store entry recorded (library
/// files, include dirs) that the leak scan must allowlist.
#[derive(Debug, Clone, Copy)]
pub struct SysdepAllow<'a> {
    pub find_name: &'a str,
    pub paths: &'a [PathBuf],
}

pub struct DepBuildRequest<'a> {
    pub dep_key: &'a str,
    pub spec: &'a DependencySpec,
    /// Raw store source tree (the checkout root — patches, when present,
    /// were already applied here by fetch).
    pub source_dir: &'a Path,
    /// Precomputed by the orchestrator (hashing::config_hash).
    pub config_hash: &'a str,
    /// Artifact store entry dir (build happens in <entry>/build-tmp, install
    /// into <entry>/install; caller marks complete).
    pub entry_dir: &'a Path,
    pub config: BuildConfig,
    pub toolchain: &'a Toolchain,
    pub abi_flags: &'a [String],
    /// Install dirs of the transitive needs closure, topo order.
    pub prefix_path: &'a [PathBuf],
    /// A.5 (wave 1): configure root = `source_dir.join(subdir)` when set —
    /// the literal `subdir` string from the dep declaration. `None` is
    /// byte-identical v0 behavior.
    pub subdir: Option<&'a str>,
    /// §5.3/§5.5 (wave 1): declared system dependencies reachable from this
    /// dep's configure, allowed through the hermetic find restrictions.
    pub sysdep_allow: &'a [SysdepAllow<'a>],
    /// §5.5 (wave 1): downgrade leak-scan hits from errors to warnings
    /// (`cpp-pkg build --allow-undeclared-system-libs`; documented as
    /// unsupported-for-sharing).
    pub allow_undeclared_system_libs: bool,
}

/// CMake's spelling of each build configuration (shortcut for call sites in
/// this module).
fn cmake_config_name(config: BuildConfig) -> &'static str {
    config.cmake_name()
}

/// Escape a string for inclusion inside a double-quoted CMake string literal.
fn cmake_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Write the generated toolchain file into `dir`, returning its path.
/// Deterministic content for identical inputs.
pub fn write_toolchain_file(
    dir: &Path,
    toolchain: &Toolchain,
    abi_flags: &[String],
) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("creating toolchain file directory {}", dir.display()))?;

    let mut content = String::new();
    content.push_str(
        "# Generated by cpp-pkg. Dependency builds take their compilers from this\n\
         # file only; the invoking environment is deliberately scrubbed so that a\n\
         # store entry's config hash describes the toolchain actually used.\n",
    );
    content.push_str(&format!(
        "set(CMAKE_C_COMPILER \"{}\")\n",
        cmake_quote(&toolchain.cc.to_string_lossy())
    ));
    content.push_str(&format!(
        "set(CMAKE_CXX_COMPILER \"{}\")\n",
        cmake_quote(&toolchain.cxx.to_string_lossy())
    ));
    content.push_str(&format!(
        "set(CMAKE_AR \"{}\")\n",
        cmake_quote(&toolchain.ar.to_string_lossy())
    ));
    if let Some(sdk) = &toolchain.sdk_path {
        content.push_str(&format!(
            "set(CMAKE_OSX_SYSROOT \"{}\")\n",
            cmake_quote(&sdk.to_string_lossy())
        ));
    }
    if !abi_flags.is_empty() {
        // set(), not string(APPEND): CMake re-includes the toolchain file for
        // every try-compile project, and APPEND would duplicate the flags on
        // each inclusion.
        //
        // Flags route by language: -stdlib=* is meaningful only to the C++
        // driver — the C driver warns "argument unused during compilation",
        // which deps building C sources under -Werror promote to hard
        // failures. The rest of the ABI class (defines, -f*abi*) applies to
        // both drivers.
        let c_flags: Vec<&str> = abi_flags
            .iter()
            .filter(|f| !f.starts_with("-stdlib="))
            .map(String::as_str)
            .collect();
        if !c_flags.is_empty() {
            let joined = cmake_quote(&c_flags.join(" "));
            content.push_str(&format!("set(CMAKE_C_FLAGS_INIT \"{joined}\")\n"));
        }
        let joined = cmake_quote(&abi_flags.join(" "));
        content.push_str(&format!("set(CMAKE_CXX_FLAGS_INIT \"{joined}\")\n"));
    }

    let path = dir.join("cppkg-toolchain.cmake");
    fs::write(&path, content)
        .with_context(|| format!("writing toolchain file {}", path.display()))?;
    Ok(path)
}

/// The scrubbed environment used for every cmake/ninja child process.
///
/// Allowlist, not denylist: everything is dropped except the few variables
/// the tools genuinely need (PATH to find cmake/ninja/compilers, HOME/TMPDIR
/// for scratch space, TERM and locale for sane output). This removes
/// CC/CXX/CPPFLAGS/CFLAGS/CXXFLAGS/LDFLAGS, every CMAKE_* variable, SDKROOT,
/// and anything else the host shell might inject into a hashed build.
pub fn scrubbed_env() -> BTreeMap<String, String> {
    const KEEP: &[&str] = &["PATH", "HOME", "TMPDIR", "TERM", "LANG", "LC_ALL", "LC_CTYPE"];
    let mut env = BTreeMap::new();
    for key in KEEP {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_string(), value);
        }
    }
    env
}

/// The full `cmake` configure argv (minus the program itself). Split out so
/// the ordering rules are unit-testable: our defaults come first and the
/// dep's literal options last, so options can override defaults (notably
/// BUILD_SHARED_LIBS) — with repeated -D definitions, CMake's last one wins.
/// A.5: the directory CMake configures — the checkout root, or the declared
/// `subdir` inside it. Patches (§5.2) were applied at the checkout root by
/// fetch before any of this, so a patched file under the subdir is already in
/// place here.
fn configure_root(req: &DepBuildRequest) -> PathBuf {
    match req.subdir {
        Some(sub) => req.source_dir.join(sub),
        None => req.source_dir.to_path_buf(),
    }
}

fn configure_args(
    req: &DepBuildRequest,
    toolchain_file: &Path,
    build_dir: &Path,
    install_dir: &Path,
) -> Vec<String> {
    let mut args = vec![
        "-S".to_string(),
        configure_root(req).display().to_string(),
        "-B".to_string(),
        build_dir.display().to_string(),
        "-G".to_string(),
        "Ninja".to_string(),
        format!("-DCMAKE_BUILD_TYPE={}", cmake_config_name(req.config)),
        format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain_file.display()),
        format!("-DCMAKE_INSTALL_PREFIX={}", install_dir.display()),
        "-DBUILD_SHARED_LIBS=OFF".to_string(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_string(),
    ];
    args.extend(find_control_args());
    if !req.prefix_path.is_empty() {
        let joined = req
            .prefix_path
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(";");
        args.push(format!("-DCMAKE_PREFIX_PATH={joined}"));
    }
    for (key, value) in &req.spec.options {
        args.push(format!("-D{key}={value}"));
    }
    args
}

/// Find-control variables CppPkg owns for every configure it runs (§5): the
/// user and system package registries are per-host caches that would resolve
/// find_package silently outside the store, and environment-derived prefixes
/// are disabled for symmetry with the scrubbed environment. PATH-based
/// lookups (CMAKE_FIND_USE_SYSTEM_ENVIRONMENT_PATH) stay enabled — deps
/// legitimately find_program() their build tools.
///
/// §5.3 (wave 1), parameterized on declared system dependencies. The closed
/// routes above are backdoors to arbitrary per-host build trees and stay
/// closed even for declared sysdeps: a `system = true` dep is resolved from
/// the standard system prefixes, which these switches never blocked. The
/// per-package "opening" of the hermetic gate is therefore realized at the
/// leak scan (`SysdepAllow`), not in the argv — the parameter exists so
/// per-package switches (e.g. the reserved pkg-config resolution mode) can
/// land without another signature change. CMAKE_FIND_PACKAGE_PREFER_CONFIG
/// and friends are deliberately untouched. Output is byte-identical to v0
/// for every input.
pub fn find_control_args_for(_sysdep_find_names: &[&str]) -> Vec<String> {
    vec![
        "-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF".to_string(),
        "-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF".to_string(),
        "-DCMAKE_FIND_USE_CMAKE_ENVIRONMENT_PATH=OFF".to_string(),
    ]
}

/// v0 spelling: no declared system dependencies.
pub fn find_control_args() -> Vec<String> {
    find_control_args_for(&[])
}

/// Alias for the implementation plan's alternate spelling (used by the
/// sysdep probe in probe.rs).
pub fn find_control_args_for_sysdep(sysdep_find_names: &[&str]) -> Vec<String> {
    find_control_args_for(sysdep_find_names)
}

/// What the leak engine scans for. `ConfigDirOnly` is the exact v0 scope
/// (config-mode `<pkg>_DIR` results); `Extended` (§5.5 layer 2, wave 1) adds
/// the `*_LIBRARY`/`*_INCLUDE_DIR` cache shapes — the find_library route the
/// cpptrace zstd leak used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeakScope {
    ConfigDirOnly,
    Extended,
}

/// The shared cache-scanning engine behind `check_find_package_leaks` (v0
/// scope) and `scan_find_package_leaks` (wave-1 scope). Returns fully
/// formatted leak messages; the caller decides error vs warning.
fn leak_scan(
    dep_key: &str,
    cmake_cache: &Path,
    allowed_roots: &[PathBuf],
    allow: &[SysdepAllow],
    scope: LeakScope,
) -> Result<Vec<String>> {
    let text = match fs::read_to_string(cmake_cache) {
        Ok(t) => t,
        // No cache (configure variant that never wrote one): nothing to scan.
        Err(_) => return Ok(Vec::new()),
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    // Declared-sysdep machine paths are hash-covered (cppkg-sysdep-v1), so
    // they join the allowed roots outright.
    let mut allowed: Vec<PathBuf> = allowed_roots.iter().map(|p| canon(p)).collect();
    for a in allow {
        allowed.extend(a.paths.iter().map(|p| canon(p)));
    }
    // Case-insensitive: module-mode cache vars conventionally upper-case the
    // package name (find_package(Zstd) -> ZSTD_LIBRARY).
    let name_allowed =
        |stem: &str| allow.iter().any(|a| a.find_name.eq_ignore_ascii_case(stem));

    let mut leaks = Vec::new();
    for line in text.lines() {
        let Some((name_type, value)) = line.split_once('=') else {
            continue;
        };
        let Some((name, ty)) = name_type.split_once(':') else {
            continue;
        };
        if ty != "PATH" && ty != "FILEPATH" {
            continue;
        }
        if value.is_empty() || value.ends_with("-NOTFOUND") {
            continue;
        }

        // §5.5 layer 2 extended shapes, checked before the generic `_DIR`
        // suffix (`<x>_INCLUDE_DIR` also ends in `_DIR`).
        if scope == LeakScope::Extended
            && let Some(stem) = name
                .strip_suffix("_INCLUDE_DIR")
                .or_else(|| name.strip_suffix("_LIBRARY"))
        {
            if stem.is_empty() || !Path::new(value).is_absolute() {
                continue;
            }
            if name_allowed(stem) {
                continue;
            }
            let path = canon(Path::new(value));
            if allowed.iter().any(|root| path.starts_with(root)) {
                continue;
            }
            leaks.push(format!(
                "while configuring dependency '{dep_key}', the configure recorded \
                 {name} = {value}, which is outside the cpp-pkg store and not covered \
                 by a declared system dependency — the build would silently consume an \
                 unmanaged system library whose contents no hash input describes.\n\
                 Declare it in CppPkg.toml ([dependencies.{stem_lc}] system = true), \
                 declare it as a fetched (git/url) dependency, or disable the feature \
                 that probes for it.",
                stem_lc = stem.to_lowercase(),
            ));
            continue;
        }

        let Some(pkg) = name.strip_suffix("_DIR") else {
            continue;
        };
        if pkg.is_empty() {
            continue;
        }
        let dir = Path::new(value);
        // Only genuine config-mode results: the dir must hold the package's
        // config file. This filters FOO_INCLUDE_DIR-style cache entries that
        // merely end in _DIR (handled by the Extended scope above; in v0
        // scope, module-mode results may legitimately point at platform SDK
        // paths).
        let has_config = dir.join(format!("{pkg}Config.cmake")).is_file()
            || dir
                .join(format!("{}-config.cmake", pkg.to_lowercase()))
                .is_file();
        if !has_config {
            continue;
        }
        if name_allowed(pkg) {
            continue;
        }
        let dir = canon(dir);
        if allowed.iter().any(|root| dir.starts_with(root)) {
            continue;
        }
        leaks.push(format!(
            "while configuring dependency '{dep_key}', find_package({pkg}) resolved to \
             {value}, which is outside the cpp-pkg store — the build would silently \
             consume an unmanaged system package (its contents are not part of this \
             store entry's config hash).\n\
             Declare \"{pkg}\" under [dependencies] in CppPkg.toml: as a fetched \
             (git/url) dependency whose key is added to the `needs` list of \
             '{dep_key}' so the store-built copy is found instead, or as a declared \
             system dependency (system = true)."
        ));
    }
    Ok(leaks)
}

/// §5 leak detection (v0 surface, kept source-compatible for existing
/// callers): after a configure, every config-mode find_package result
/// (`<pkg>_DIR` cache entries whose directory really holds a
/// `<pkg>Config.cmake` / `<pkg>-config.cmake`) must lie under one of
/// `allowed_roots` (store prefixes + the dep's own trees). A hit outside
/// means the configure silently consumed an unmanaged system package — e.g.
/// a Homebrew copy found because its bin dir is on PATH — whose contents the
/// store entry's config hash does not describe. Errors on the first leak.
pub fn check_find_package_leaks(
    dep_key: &str,
    cmake_cache: &Path,
    allowed_roots: &[PathBuf],
) -> Result<()> {
    let leaks = leak_scan(dep_key, cmake_cache, allowed_roots, &[], LeakScope::ConfigDirOnly)?;
    match leaks.into_iter().next() {
        Some(leak) => Err(anyhow!("{leak}")),
        None => Ok(()),
    }
}

/// §5.5 layer 2 (wave 1): the full-scope scan — config-mode `_DIR` results
/// plus `*_LIBRARY`/`*_INCLUDE_DIR` cache shapes — with the declared-sysdep
/// allowlist. Returns the leak messages instead of erroring so the caller
/// implements the error-by-default / `--allow-undeclared-system-libs`
/// downgrade policy.
pub fn scan_find_package_leaks(
    dep_key: &str,
    cmake_cache: &Path,
    allowed_roots: &[PathBuf],
    allow: &[SysdepAllow],
) -> Result<Vec<String>> {
    leak_scan(dep_key, cmake_cache, allowed_roots, allow, LeakScope::Extended)
}

/// A.10: after a successful configure, warn about dep `options` keys the
/// project never declared. Every `-D` lands in the cache; ones no
/// `option()`/`set(CACHE)` declared stay type UNINITIALIZED — the footprint
/// of a misspelled or version-mismatched option. `CMAKE_*` keys are read by
/// CMake itself without ever being "declared", so they are exempt. Warning
/// only: projects can read a variable (`if(DEFINED …)`) without declaring it.
fn lint_unknown_options(options: &BTreeMap<String, String>, cache_text: &str) -> Vec<String> {
    let mut types: BTreeMap<&str, &str> = BTreeMap::new();
    for line in cache_text.lines() {
        if line.starts_with("//") || line.trim().is_empty() {
            continue;
        }
        if let Some((name_ty, _)) = line.split_once('=')
            && let Some((name, ty)) = name_ty.rsplit_once(':')
        {
            types.insert(name, ty);
        }
    }
    let mut warnings = Vec::new();
    for key in options.keys() {
        // Users may spell a type into the key (`FOO:BOOL`); the cache entry
        // name is the bare part.
        let bare = key.split(':').next().unwrap_or(key);
        if bare.starts_with("CMAKE_") {
            continue;
        }
        match types.get(bare) {
            Some(&"UNINITIALIZED") => warnings.push(format!(
                "option '{bare}' was not declared by the dependency's CMake project \
                 (it stayed UNINITIALIZED in the cache) — possibly misspelled, or \
                 unknown at this pin"
            )),
            None => warnings.push(format!(
                "option '{bare}' did not appear in the CMake cache after configure — \
                 possibly misspelled"
            )),
            Some(_) => {}
        }
    }
    warnings
}

/// Run a command with the scrubbed env, writing combined stdout+stderr to
/// `log_path`. Returns (success, combined output).
fn run_logged(
    mut cmd: Command,
    env: &BTreeMap<String, String>,
    log_path: &Path,
) -> Result<(bool, String)> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.env_clear().envs(env);
    let out = cmd
        .output()
        .with_context(|| format!("failed to run `{program}` (is it installed and on PATH?)"))?;
    let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    fs::write(log_path, &log).with_context(|| format!("writing log {}", log_path.display()))?;
    Ok((out.status.success(), log))
}

/// Configure + build + install one dependency. Skips nothing: the caller
/// checks store completeness before calling.
pub fn build_dependency(req: &DepBuildRequest) -> Result<BuiltDep> {
    // A.5: validate the declared subdir before spending a configure on it.
    // Schema validation already polices the spelling; this is the defense at
    // the point where the path is actually used.
    if let Some(sub) = req.subdir {
        let p = Path::new(sub);
        if p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!(
                "dependency '{}': subdir \"{sub}\" must be a relative path inside \
                 the checkout (no leading '/', no '..')",
                req.dep_key
            );
        }
        let root = configure_root(req);
        if !root.join("CMakeLists.txt").is_file() {
            bail!(
                "dependency '{}': subdir \"{sub}\" does not contain a CMakeLists.txt \
                 (looked in {})",
                req.dep_key,
                root.display()
            );
        }
    }

    let build_dir = req.entry_dir.join("build-tmp");
    if build_dir.exists() {
        // A leftover tree means an earlier build was interrupted; its cache
        // may pin stale paths/options, so start clean.
        fs::remove_dir_all(&build_dir)
            .with_context(|| format!("removing stale build tree {}", build_dir.display()))?;
    }
    fs::create_dir_all(&build_dir)
        .with_context(|| format!("creating build tree {}", build_dir.display()))?;
    let install_dir = req.entry_dir.join("install");
    fs::create_dir_all(&install_dir)
        .with_context(|| format!("creating install prefix {}", install_dir.display()))?;

    let toolchain_file = write_toolchain_file(&build_dir, req.toolchain, req.abi_flags)?;
    let env = scrubbed_env();

    // Configure.
    let configure_log = build_dir.join("cppkg-configure.log");
    let mut cmd = Command::new("cmake");
    cmd.args(configure_args(req, &toolchain_file, &build_dir, &install_dir));
    let (ok, log) = run_logged(cmd, &env, &configure_log)?;
    if !ok {
        return Err(translate_configure_failure(req.dep_key, &log, &configure_log));
    }

    // A successful configure may still have found packages outside the store
    // (§5/§5.5): fail before building against them.
    let mut allowed: Vec<PathBuf> = req.prefix_path.to_vec();
    allowed.push(req.entry_dir.to_path_buf());
    allowed.push(req.source_dir.to_path_buf());
    if let Some(sdk) = &req.toolchain.sdk_path {
        // The toolchain sysroot is an allowed root for the extended shapes:
        // module-mode finds legitimately land inside the SDK (curl's
        // FindZLIB resolves ZLIB_INCLUDE_DIR to <sdk>/usr/include), and the
        // SDK's contents ARE covered by a hash input — ToolchainIdentity's
        // sdk_version is part of every config hash — which is the §5.5
        // invariant's actual test. (Deviation from the spec's "SDK-rooted
        // paths are not exempt" sentence, recorded in the wave-1 report:
        // without this, every v0-green macOS project using an SDK-provided
        // library re-keys from green to error.)
        allowed.push(sdk.clone());
    }
    let cache_path = build_dir.join("CMakeCache.txt");
    let leaks = scan_find_package_leaks(req.dep_key, &cache_path, &allowed, req.sysdep_allow)?;
    if !leaks.is_empty() {
        if req.allow_undeclared_system_libs {
            for leak in &leaks {
                eprintln!(
                    "cpp-pkg: warning: {leak}\n(continuing under \
                     --allow-undeclared-system-libs; this build is unsupported for \
                     sharing)"
                );
            }
        } else {
            bail!("{}", leaks.join("\n\n"));
        }
    }

    // A.10: advisory lint — options the project never declared.
    if !req.spec.options.is_empty() {
        let cache_text = fs::read_to_string(&cache_path).unwrap_or_default();
        for warning in lint_unknown_options(&req.spec.options, &cache_text) {
            eprintln!("cpp-pkg: warning ({}): {warning}", req.dep_key);
        }
    }

    // Build.
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let build_log = build_dir.join("cppkg-build.log");
    let mut cmd = Command::new("cmake");
    cmd.arg("--build")
        .arg(&build_dir)
        .arg("--parallel")
        .arg(jobs.to_string());
    let (ok, _) = run_logged(cmd, &env, &build_log)?;
    if !ok {
        return Err(anyhow!(
            "cmake --build failed for dependency '{}'.\nFull build log: {}",
            req.dep_key,
            build_log.display()
        ));
    }

    // Install.
    let install_log = build_dir.join("cppkg-install.log");
    let mut cmd = Command::new("cmake");
    cmd.arg("--install").arg(&build_dir);
    let (ok, _) = run_logged(cmd, &env, &install_log)?;
    if !ok {
        return Err(anyhow!(
            "cmake --install failed for dependency '{}'.\nFull install log: {}",
            req.dep_key,
            install_log.display()
        ));
    }

    // The build tree (and its logs) only matter for debugging failures; the
    // store entry keeps just the install prefix.
    fs::remove_dir_all(&build_dir)
        .with_context(|| format!("removing build tree {}", build_dir.display()))?;

    Ok(BuiltDep {
        dep_key: req.dep_key.to_string(),
        config_hash: req.config_hash.to_string(),
        install_dir,
    })
}

// ---------------------------------------------------------------------------
// Configure-failure translation
// ---------------------------------------------------------------------------

/// CMake wraps error paragraphs at ~70 columns with two-space continuation
/// indents, which can split phrases like `provided by "X"` across lines.
/// Re-join continuation lines into single-line paragraphs so phrase matching
/// is reliable.
fn normalize_wrapped(log: &str) -> String {
    let mut paras: Vec<String> = Vec::new();
    for line in log.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().is_empty() {
            paras.push(String::new());
            continue;
        }
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');
        if is_continuation
            && let Some(last) = paras.last_mut()
                && !last.is_empty() {
                    last.push(' ');
                    last.push_str(trimmed.trim_start());
                    continue;
                }
        paras.push(trimmed.trim_start().to_string());
    }
    paras.join("\n")
}

/// The text between `marker` (which should end with the opening `"`) and the
/// next `"`.
fn extract_quoted<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let idx = text.find(marker)?;
    let rest = &text[idx + marker.len()..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// A.9: CMake ≥ 4 refuses to configure projects whose cmake_minimum_required
/// declares < 3.5 ("Compatibility with CMake < 3.5 has been removed from
/// CMake"). Our configure passes CMAKE_POLICY_VERSION_MINIMUM=3.5 by default,
/// so this shape surfaces mainly from nested configures (ExternalProject and
/// friends, which do not inherit the -D) or when a dep option overrides the
/// default — translate it into the blessed options incantation either way.
fn detect_policy_refusal(dep_key: &str, norm: &str) -> Option<String> {
    if !(norm.contains("Compatibility with CMake <")
        && norm.contains("has been removed from CMake"))
    {
        return None;
    }
    Some(format!(
        "While configuring dependency '{dep_key}', CMake refused the project's \
         cmake_minimum_required version: CMake 4 removed compatibility with CMake \
         < 3.5, and this pin declares an older minimum.\n\
         Add the policy floor as an ordinary option of this dependency in \
         CppPkg.toml:\n\
         \x20 [dependencies.{dep_key}.options]\n\
         \x20 CMAKE_POLICY_VERSION_MINIMUM = \"3.5\""
    ))
}

/// Version-rejection shape: a config file for the package exists but its
/// version was rejected (find_package(X <ver>) or find_dependency(X <ver>)).
/// This is CMake's most confusing failure mode — name requested vs available.
fn detect_version_rejection(dep_key: &str, norm: &str, raw: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "that is compatible with requested version",
        "requires at least version",
        "required is at least",
        "no suitable version",
        "Found unsuitable version",
    ];
    if !MARKERS.iter().any(|m| norm.contains(m)) {
        return None;
    }
    let pkg = extract_quoted(norm, "for package \"")
        .or_else(|| extract_quoted(norm, "provided by \""))
        .or_else(|| extract_quoted(norm, "Could NOT find \""))
        .unwrap_or("<unknown package>");
    let requested = extract_quoted(norm, "requested version \"")
        .or_else(|| extract_quoted(norm, "required is at least \""))
        .unwrap_or("<unknown>");

    // The "configuration files were considered but not accepted" block lists
    // each candidate as `<path>, version: <v>`; those are the versions the
    // store actually provides.
    let mut available: Vec<String> = Vec::new();
    for line in raw.lines() {
        if let Some(idx) = line.find(", version: ") {
            let v = line[idx + ", version: ".len()..].trim();
            if !v.is_empty() && !available.iter().any(|a| a == v) {
                available.push(v.to_string());
            }
        }
    }
    if available.is_empty()
        && let Some(v) = extract_quoted(norm, "Found unsuitable version \"") {
            available.push(v.to_string());
        }
    let available_txt = if available.is_empty() {
        "none that CMake accepted".to_string()
    } else {
        available.join(", ")
    };

    Some(format!(
        "While configuring dependency '{dep_key}', CMake rejected package \"{pkg}\" on \
         version grounds: requested version {requested}, available version(s): {available_txt}.\n\
         The store provides exactly the version pinned in CppPkg.toml / CppPkg.lock — update \
         the pin of the dependency that provides \"{pkg}\" (or relax the requirement in \
         '{dep_key}') so the two agree."
    ))
}

/// Not-found shape: `find_package`/`find_dependency` could not locate a
/// package at all. Covers config-mode ("Could not find a package
/// configuration file provided by \"X\"", also emitted for find_dependency)
/// and module-mode ("Could NOT find X").
fn detect_not_found(dep_key: &str, norm: &str) -> Option<String> {
    let pkg: String = extract_quoted(norm, "package configuration file provided by \"")
        .map(str::to_string)
        .or_else(|| {
            let idx = norm.find("Could NOT find ")?;
            let rest = &norm[idx + "Could NOT find ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            (!name.is_empty()).then_some(name)
        })?;
    let origin = if norm.contains("(find_dependency)") {
        format!(" (required via find_dependency from one of '{dep_key}'s own CMake config files)")
    } else {
        String::new()
    };
    let suggested_key = pkg.to_lowercase();
    Some(format!(
        "While configuring dependency '{dep_key}', CMake could not find package \"{pkg}\"{origin}.\n\
         If \"{pkg}\" is provided by a CMake package, declare it under [dependencies] in \
         CppPkg.toml and add its key to the `needs` list of '{dep_key}' \
         (e.g. needs = [\"{suggested_key}\"]) so its install prefix is placed on \
         CMAKE_PREFIX_PATH for this build."
    ))
}

/// Translate a failed configure into an actionable error. The policy refusal
/// (A.9) is checked first (unambiguous text), then version rejection (its
/// log can also contain not-found phrasing), then not-found; the raw log
/// path is always included.
fn translate_configure_failure(dep_key: &str, log: &str, log_path: &Path) -> anyhow::Error {
    let norm = normalize_wrapped(log);
    let base = detect_policy_refusal(dep_key, &norm)
        .or_else(|| detect_version_rejection(dep_key, &norm, log))
        .or_else(|| detect_not_found(dep_key, &norm))
        .unwrap_or_else(|| format!("CMake configure failed for dependency '{dep_key}'."));
    anyhow!("{base}\nFull CMake configure log: {}", log_path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::{Dialect, ToolchainIdentity};

    // Integration-style tests that drive real cmake/ninja live in
    // tests/cmakebuild_test.rs (public API only); this module covers the
    // private helpers.

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

    /// Build a DependencySpec by parsing a manifest instead of a struct
    /// literal, so schema-bundle field additions don't break this module.
    fn dummy_spec(options: BTreeMap<String, String>) -> DependencySpec {
        let mut toml = String::from(
            "schema-version = 1\n\
             [package]\nname = \"t\"\nversion = \"0.1.0\"\n\
             [dependencies.hello]\n\
             git = \"https://example.invalid/hello.git\"\n\
             tag = \"v1.0.0\"\n",
        );
        if !options.is_empty() {
            toml.push_str("[dependencies.hello.options]\n");
            for (k, v) in &options {
                toml.push_str(&format!("{k} = \"{v}\"\n"));
            }
        }
        let (project, _) = crate::schema::parse_str(&toml).expect("test manifest parses");
        project.dependencies.get("hello").expect("hello dep").clone()
    }

    #[test]
    fn cmakebuild_configure_args_defaults_before_dep_options() {
        let tc = test_toolchain();
        let mut options = BTreeMap::new();
        options.insert("BUILD_SHARED_LIBS".to_string(), "ON".to_string());
        options.insert("HELLO_OPT".to_string(), "1".to_string());
        let spec = dummy_spec(options);
        let prefixes = vec![PathBuf::from("/store/a/install"), PathBuf::from("/store/b/install")];
        let req = DepBuildRequest {
            dep_key: "hello",
            spec: &spec,
            source_dir: Path::new("/src"),
            config_hash: "cafe",
            entry_dir: Path::new("/entry"),
            config: BuildConfig::Debug,
            toolchain: &tc,
            abi_flags: &[],
            prefix_path: &prefixes,
            subdir: None,
            sysdep_allow: &[],
            allow_undeclared_system_libs: false,
        };
        let args = configure_args(&req, Path::new("/entry/build-tmp/cppkg-toolchain.cmake"),
                                  Path::new("/entry/build-tmp"), Path::new("/entry/install"));

        let pos = |needle: &str| args.iter().position(|a| a == needle)
            .unwrap_or_else(|| panic!("missing arg {needle}: {args:?}"));
        // Dep options come after our default so CMake's last-wins lets deps
        // override BUILD_SHARED_LIBS.
        assert!(pos("-DBUILD_SHARED_LIBS=OFF") < pos("-DBUILD_SHARED_LIBS=ON"));
        assert!(args.contains(&"-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_string()));
        // §5 find-control ownership: registries and env-derived prefixes off.
        assert!(args.contains(&"-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF".to_string()));
        assert!(args.contains(&"-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF".to_string()));
        assert!(args.contains(&"-DCMAKE_FIND_USE_CMAKE_ENVIRONMENT_PATH=OFF".to_string()));
        assert!(args.contains(&"-DCMAKE_BUILD_TYPE=Debug".to_string()));
        assert!(args.contains(&"-DHELLO_OPT=1".to_string()));
        assert!(args.contains(&"-DCMAKE_PREFIX_PATH=/store/a/install;/store/b/install".to_string()));
        assert!(args.contains(&"-G".to_string()) && args.contains(&"Ninja".to_string()));
    }

    #[test]
    fn cmakebuild_toolchain_file_routes_stdlib_to_cxx_only() {
        let tmp = tempfile::tempdir().unwrap();
        let tc = test_toolchain();
        let flags = vec![
            "-stdlib=libc++".to_string(),
            "-D_GLIBCXX_ASSERTIONS".to_string(),
        ];
        let path = write_toolchain_file(tmp.path(), &tc, &flags).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("set(CMAKE_CXX_FLAGS_INIT \"-stdlib=libc++ -D_GLIBCXX_ASSERTIONS\")"));
        assert!(text.contains("set(CMAKE_C_FLAGS_INIT \"-D_GLIBCXX_ASSERTIONS\")"));

        // Only C++-only flags: no C_FLAGS_INIT line at all.
        let only = vec!["-stdlib=libc++".to_string()];
        let path = write_toolchain_file(&tmp.path().join("b"), &tc, &only).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("CMAKE_CXX_FLAGS_INIT"));
        assert!(!text.contains("CMAKE_C_FLAGS_INIT \""));
    }

    #[test]
    fn cmakebuild_leak_check_flags_out_of_store_config_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store/fmt-abc/install/lib/cmake/fmt");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("fmtConfig.cmake"), "").unwrap();
        let brew = tmp.path().join("brew/lib/cmake/spdlog");
        std::fs::create_dir_all(&brew).unwrap();
        // Lowercase spelling proves both config-file names are recognized.
        std::fs::write(brew.join("spdlog-config.cmake"), "").unwrap();
        let sysdir = tmp.path().join("sys/include");
        std::fs::create_dir_all(&sysdir).unwrap();

        let allowed = vec![tmp.path().join("store")];
        let cache = tmp.path().join("CMakeCache.txt");
        let write_cache = |lines: &[String]| std::fs::write(&cache, lines.join("\n")).unwrap();

        // In-store result, module-mode-ish _DIR without a config file, and a
        // NOTFOUND entry: all fine.
        write_cache(&[
            format!("fmt_DIR:PATH={}", store.display()),
            format!("ZLIB_INCLUDE_DIR:PATH={}", sysdir.display()),
            "curl_DIR:PATH=curl_DIR-NOTFOUND".to_string(),
            "CMAKE_BUILD_TYPE:STRING=Release".to_string(),
        ]);
        check_find_package_leaks("dep", &cache, &allowed).unwrap();

        // A config-mode hit outside the store must fail, naming the package.
        write_cache(&[format!("spdlog_DIR:PATH={}", brew.display())]);
        let err = check_find_package_leaks("dep", &cache, &allowed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("spdlog"), "{err}");
        assert!(err.contains("outside the cpp-pkg store"), "{err}");
        assert!(err.contains("[dependencies]") && err.contains("needs"), "{err}");

        // Missing cache file is not an error.
        check_find_package_leaks("dep", &tmp.path().join("nope.txt"), &allowed).unwrap();
    }

    #[test]
    fn cmakebuild_translates_not_found_from_canned_log() {
        // Wrapped exactly the way CMake wraps config-mode not-found errors.
        let log = "\
-- Configuring incomplete, errors occurred!
CMake Error at CMakeLists.txt:3 (find_package):
  By not providing \"Findfmt.cmake\" in CMAKE_MODULE_PATH this project has
  asked CMake to find a package configuration file provided by \"fmt\", but
  CMake did not find one.

  Could not find a package configuration file provided by \"fmt\" with any of
  the following names:

    fmtConfig.cmake
    fmt-config.cmake
";
        let err = translate_configure_failure("spdlog", log, Path::new("/e/build-tmp/cppkg-configure.log"));
        let msg = format!("{err}");
        assert!(msg.contains("\"fmt\""), "must name the missing package: {msg}");
        assert!(msg.contains("[dependencies]"), "must suggest [dependencies]: {msg}");
        assert!(msg.contains("needs"), "must suggest needs: {msg}");
        assert!(msg.contains("'spdlog'"), "must name the dep being configured: {msg}");
        assert!(msg.contains("/e/build-tmp/cppkg-configure.log"), "must include log path: {msg}");
    }

    #[test]
    fn cmakebuild_translates_find_dependency_origin() {
        let log = "\
CMake Error at /store/spdlog/install/lib/cmake/spdlog/spdlogConfig.cmake:12 (find_dependency):
  Could not find a package configuration file provided by \"fmt\" with any of
  the following names:

    fmtConfig.cmake
Call Stack (most recent call first):
  CMakeLists.txt:4 (find_package)
";
        let err = translate_configure_failure("spdlog", log, Path::new("/log"));
        let msg = format!("{err}");
        assert!(msg.contains("find_dependency"), "must mention find_dependency origin: {msg}");
        assert!(msg.contains("\"fmt\""));
    }

    #[test]
    fn cmakebuild_translates_version_rejection_from_canned_log() {
        let log = "\
CMake Error at CMakeLists.txt:3 (find_package):
  Could not find a configuration file for package \"fmt\" that is compatible
  with requested version \"99.0\".

  The following configuration files were considered but were not accepted:

    /store/fmt/install/lib/cmake/fmt/fmt-config.cmake, version: 11.2.0
";
        let err = translate_configure_failure("spdlog", log, Path::new("/log"));
        let msg = format!("{err}");
        assert!(msg.contains("\"fmt\""), "must name the package: {msg}");
        assert!(msg.contains("99.0"), "must name the requested version: {msg}");
        assert!(msg.contains("11.2.0"), "must name the available version: {msg}");
        assert!(msg.contains("/log"), "must include log path: {msg}");
    }

    #[test]
    fn cmakebuild_normalize_rejoins_wrapped_paragraphs() {
        let wrapped = "CMake Error at CMakeLists.txt:3 (find_package):\n  asked CMake to find a package configuration file provided by\n  \"fmt\", but CMake did not find one.\n";
        let norm = normalize_wrapped(wrapped);
        assert!(
            norm.contains("configuration file provided by \"fmt\""),
            "wrapped phrase must be rejoined: {norm}"
        );
    }

    #[test]
    fn cmakebuild_subdir_joins_configure_root() {
        let tc = test_toolchain();
        let spec = dummy_spec(BTreeMap::new());
        let req = DepBuildRequest {
            dep_key: "zstd",
            spec: &spec,
            source_dir: Path::new("/src/zstd"),
            config_hash: "cafe",
            entry_dir: Path::new("/entry"),
            config: BuildConfig::Release,
            toolchain: &tc,
            abi_flags: &[],
            prefix_path: &[],
            subdir: Some("build/cmake"),
            sysdep_allow: &[],
            allow_undeclared_system_libs: false,
        };
        let args = configure_args(
            &req,
            Path::new("/entry/build-tmp/cppkg-toolchain.cmake"),
            Path::new("/entry/build-tmp"),
            Path::new("/entry/install"),
        );
        let s_pos = args.iter().position(|a| a == "-S").expect("-S present");
        assert_eq!(args[s_pos + 1], "/src/zstd/build/cmake");

        // No subdir: v0-identical configure root.
        let req_none = DepBuildRequest { subdir: None, ..req };
        let args = configure_args(
            &req_none,
            Path::new("/entry/build-tmp/cppkg-toolchain.cmake"),
            Path::new("/entry/build-tmp"),
            Path::new("/entry/install"),
        );
        let s_pos = args.iter().position(|a| a == "-S").unwrap();
        assert_eq!(args[s_pos + 1], "/src/zstd");
    }

    #[test]
    fn cmakebuild_find_control_args_stable_across_sysdep_names() {
        // §5.3: the closed find routes stay closed even for declared
        // sysdeps; the argv is byte-identical to v0 for every input.
        assert_eq!(find_control_args(), find_control_args_for(&[]));
        assert_eq!(find_control_args_for(&[]), find_control_args_for(&["ZLIB", "Boost"]));
        assert_eq!(
            find_control_args_for_sysdep(&["ZLIB"]),
            find_control_args_for(&["ZLIB"])
        );
    }

    #[test]
    fn cmakebuild_extended_scan_flags_library_and_include_dir_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        let brew_lib = tmp.path().join("brew/lib");
        std::fs::create_dir_all(&brew_lib).unwrap();
        std::fs::write(brew_lib.join("libzstd.dylib"), "").unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(store.join("include")).unwrap();
        let sysroot = tmp.path().join("sdk/usr/include");
        std::fs::create_dir_all(&sysroot).unwrap();

        let cache = tmp.path().join("CMakeCache.txt");
        let allowed = vec![store.clone()];
        let write_cache = |lines: &[String]| std::fs::write(&cache, lines.join("\n")).unwrap();

        // Out-of-store find_library hit: a leak naming the var, the path,
        // and both fixes.
        write_cache(&[format!(
            "ZSTD_LIBRARY:FILEPATH={}",
            brew_lib.join("libzstd.dylib").display()
        )]);
        let leaks = scan_find_package_leaks("cpptrace", &cache, &allowed, &[]).unwrap();
        assert_eq!(leaks.len(), 1, "{leaks:?}");
        assert!(leaks[0].contains("ZSTD_LIBRARY"), "{}", leaks[0]);
        assert!(leaks[0].contains("libzstd.dylib"), "{}", leaks[0]);
        assert!(leaks[0].contains("system = true"), "{}", leaks[0]);
        assert!(leaks[0].contains("'cpptrace'"), "{}", leaks[0]);

        // The v0-surface checker keeps its v0 scope: the same cache is clean
        // there (probe.rs behavior unchanged until it adopts the new scan).
        check_find_package_leaks("cpptrace", &cache, &allowed).unwrap();

        // Declared sysdep, matched by find name (case-insensitive
        // module-var convention): allowed through.
        let allow_by_name = [SysdepAllow { find_name: "zstd", paths: &[] }];
        let leaks = scan_find_package_leaks("cpptrace", &cache, &allowed, &allow_by_name).unwrap();
        assert!(leaks.is_empty(), "{leaks:?}");

        // Declared sysdep, matched by recorded machine path.
        let paths = vec![tmp.path().join("brew")];
        let allow_by_path = [SysdepAllow { find_name: "other", paths: &paths }];
        let leaks = scan_find_package_leaks("cpptrace", &cache, &allowed, &allow_by_path).unwrap();
        assert!(leaks.is_empty(), "{leaks:?}");

        // Include-dir shape under an allowed root (the sysroot case), plus
        // NOTFOUND and non-PATH types: all clean.
        write_cache(&[
            format!("ZLIB_INCLUDE_DIR:PATH={}", sysroot.display()),
            "PSL_LIBRARY:FILEPATH=PSL_LIBRARY-NOTFOUND".to_string(),
            "SOME_LIBRARY:STRING=whatever".to_string(),
            "FOO_LIBRARY:UNINITIALIZED=/nope/libfoo.a".to_string(),
        ]);
        let sys_allowed = vec![store.clone(), tmp.path().join("sdk")];
        let leaks = scan_find_package_leaks("curl", &cache, &sys_allowed, &[]).unwrap();
        assert!(leaks.is_empty(), "{leaks:?}");

        // Out-of-allowed include dir fires.
        let leaks = scan_find_package_leaks("curl", &cache, &allowed, &[]).unwrap();
        assert_eq!(leaks.len(), 1, "{leaks:?}");
        assert!(leaks[0].contains("ZLIB_INCLUDE_DIR"), "{}", leaks[0]);
    }

    #[test]
    fn cmakebuild_extended_scan_still_catches_config_dir_leaks() {
        let tmp = tempfile::tempdir().unwrap();
        let brew = tmp.path().join("brew/lib/cmake/spdlog");
        std::fs::create_dir_all(&brew).unwrap();
        std::fs::write(brew.join("spdlogConfig.cmake"), "").unwrap();
        let cache = tmp.path().join("CMakeCache.txt");
        std::fs::write(&cache, format!("spdlog_DIR:PATH={}", brew.display())).unwrap();

        let allowed = vec![tmp.path().join("store")];
        let leaks = scan_find_package_leaks("dep", &cache, &allowed, &[]).unwrap();
        assert_eq!(leaks.len(), 1, "{leaks:?}");
        assert!(leaks[0].contains("spdlog"), "{}", leaks[0]);
        assert!(leaks[0].contains("outside the cpp-pkg store"), "{}", leaks[0]);
        assert!(leaks[0].contains("system = true"), "{}", leaks[0]);

        // Declared as a sysdep by its find name: allowed.
        let allow = [SysdepAllow { find_name: "spdlog", paths: &[] }];
        let leaks = scan_find_package_leaks("dep", &cache, &allowed, &allow).unwrap();
        assert!(leaks.is_empty(), "{leaks:?}");
    }

    #[test]
    fn cmakebuild_translates_policy_refusal_from_canned_log() {
        // CMake 4's exact refusal shape for cmake_minimum_required < 3.5.
        let log = "\
CMake Error at CMakeLists.txt:1 (cmake_minimum_required):
  Compatibility with CMake < 3.5 has been removed from CMake.

  Update the VERSION argument <min> value.  Or, use the <min>...<max> syntax
  to tell CMake that the project requires at least <min> but has been updated
  to work with policies introduced by <max> or earlier.

  Or, add -DCMAKE_POLICY_VERSION_MINIMUM=3.5 to try configuring anyway.
";
        let err = translate_configure_failure("googletest", log, Path::new("/log"));
        let msg = format!("{err}");
        assert!(
            msg.contains("CMAKE_POLICY_VERSION_MINIMUM = \"3.5\""),
            "must contain the options incantation: {msg}"
        );
        assert!(
            msg.contains("[dependencies.googletest.options]"),
            "must name the dep's options table: {msg}"
        );
        assert!(msg.contains("/log"), "must include log path: {msg}");
    }

    #[test]
    fn cmakebuild_lints_undeclared_options() {
        let cache = "\
// comment line
JSON_BuildTests:BOOL=OFF
TYPO_OPTION:UNINITIALIZED=ON
CMAKE_POLICY_VERSION_MINIMUM:UNINITIALIZED=3.5
CMAKE_BUILD_TYPE:STRING=Release
";
        let mut options = BTreeMap::new();
        options.insert("JSON_BuildTests".to_string(), "OFF".to_string());
        options.insert("TYPO_OPTION".to_string(), "ON".to_string());
        // CMAKE_* keys are read by CMake itself, never "declared": exempt.
        options.insert("CMAKE_POLICY_VERSION_MINIMUM".to_string(), "3.5".to_string());
        options.insert("VANISHED".to_string(), "1".to_string());

        let warnings = lint_unknown_options(&options, cache);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.contains("TYPO_OPTION") && w.contains("UNINITIALIZED")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("VANISHED") && w.contains("did not appear")),
            "{warnings:?}"
        );
    }

    #[test]
    fn cmakebuild_rejects_escaping_or_absolute_subdir() {
        let tc = test_toolchain();
        let spec = dummy_spec(BTreeMap::new());
        let tmp = tempfile::tempdir().unwrap();
        let mk_req = |sub: &'static str| DepBuildRequest {
            dep_key: "zstd",
            spec: &spec,
            source_dir: tmp.path(),
            config_hash: "cafe",
            entry_dir: tmp.path(),
            config: BuildConfig::Release,
            toolchain: &tc,
            abi_flags: &[],
            prefix_path: &[],
            subdir: Some(sub),
            sysdep_allow: &[],
            allow_undeclared_system_libs: false,
        };
        let err = build_dependency(&mk_req("../escape")).unwrap_err().to_string();
        assert!(err.contains("relative path"), "{err}");
        let err = build_dependency(&mk_req("/abs")).unwrap_err().to_string();
        assert!(err.contains("relative path"), "{err}");
        // Present but empty of CMakeLists.txt: named clearly, before any
        // cmake process is spawned.
        std::fs::create_dir_all(tmp.path().join("build/cmake")).unwrap();
        let err = build_dependency(&mk_req("build/cmake")).unwrap_err().to_string();
        assert!(err.contains("CMakeLists.txt"), "{err}");
        assert!(err.contains("build/cmake"), "{err}");
    }
}
