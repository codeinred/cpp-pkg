//! Toolchain detection, semantic identity, and the GNU-dialect flag driver.
//! Decisions: CPP_PKG_IMPLEMENTATION.md §3 + §7, CPPKG_TOML.md (profiles).
//!
//! Detection contract (§7.3):
//! - Identify via predefined macros: run `<cxx> -dM -E -x c++ /dev/null`,
//!   parse __clang_major__/__GNUC__/__apple_build_version__ etc. NEVER parse
//!   version banners (Apple Clang vs LLVM Clang versions are unrelated).
//! - Capture: compiler id (AppleClang | Clang | GNU), version, default C++
//!   stdlib (+ version macro), target triple (`-dumpmachine`), macOS SDK path
//!   (`xcrun --show-sdk-path`) + SDK version — SDK version is part of the
//!   identity.
//! - Derive cc from cxx (clang++ -> clang, g++-15 -> gcc-15) unless preset
//!   overrides; ar likewise (prefer llvm-ar/gcc-ar-N next to the compiler,
//!   fall back to `ar` on PATH).
//! - Toolchain IDENTITY is the detection OUTPUT (semantic), never a binary
//!   hash. Detection cache (stat-keyed) is a future nicety — v0 may re-detect
//!   each run (one -dM -E is cheap).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Gnu,
    // Msvc: deferred (not v0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    C,
    Cxx,
}

/// Normalized semantic identity — the toolchain's contribution to every
/// dependency config hash. All fields are detection output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainIdentity {
    pub dialect: Dialect,
    /// "AppleClang" | "Clang" | "GNU"
    pub compiler_id: String,
    pub version: String,
    pub target_triple: String,
    /// "libc++" | "libstdc++"
    pub stdlib: String,
    pub stdlib_version: String,
    pub sdk_version: Option<String>,
}

impl ToolchainIdentity {
    /// Canonical string for hashing (stable field order, unambiguous
    /// separators). Changing this format invalidates every store entry —
    /// bump store::SCHEMA_VERSION marker semantics if it ever changes.
    pub fn hash_input(&self) -> String {
        // One "key=value" per line: the newline separator cannot appear in
        // any field (all values come from single-line tool output), so the
        // encoding is injective without escaping. The leading version tag
        // lets a future format change coexist with old hashes detectably.
        let dialect = match self.dialect {
            Dialect::Gnu => "gnu",
        };
        format!(
            "cppkg-toolchain-identity-v1\n\
             dialect={}\n\
             compiler-id={}\n\
             version={}\n\
             target={}\n\
             stdlib={}\n\
             stdlib-version={}\n\
             sdk-version={}\n",
            dialect,
            self.compiler_id,
            self.version,
            self.target_triple,
            self.stdlib,
            self.stdlib_version,
            self.sdk_version.as_deref().unwrap_or("none"),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub cxx: PathBuf,
    pub cc: PathBuf,
    pub ar: PathBuf,
    pub sdk_path: Option<PathBuf>,
    pub identity: ToolchainIdentity,
}

/// Detect from a C++ compiler path or command name (PATH-resolved).
pub fn detect(cxx: &str) -> Result<Toolchain> {
    let cxx_path = resolve_command(cxx)
        .with_context(|| format!("C++ compiler `{cxx}` not found (not a file, not on PATH)"))?;

    let macros = macro_dump(&cxx_path, "")
        .with_context(|| format!("failed to run `{} -dM -E -x c++ /dev/null`", cxx_path.display()))?;

    // Order matters: Apple Clang defines __clang_major__ AND __GNUC__ (as 4,
    // for compat), and upstream Clang also defines __GNUC__ — so the checks
    // go most-specific first.
    let (compiler_id, version) = if macros.contains_key("__apple_build_version__") {
        ("AppleClang".to_string(), clang_version(&macros)?)
    } else if macros.contains_key("__clang_major__") {
        ("Clang".to_string(), clang_version(&macros)?)
    } else if macros.contains_key("__GNUC__") {
        let version = format!(
            "{}.{}.{}",
            macro_value(&macros, "__GNUC__")?,
            macro_value(&macros, "__GNUC_MINOR__")?,
            macro_value(&macros, "__GNUC_PATCHLEVEL__")?,
        );
        ("GNU".to_string(), version)
    } else {
        bail!(
            "unrecognized C++ compiler `{}`: predefined macros show neither \
             __clang_major__ nor __GNUC__ (only GNU-dialect compilers are \
             supported in v0)",
            cxx_path.display()
        );
    };

    let target_triple = dumpmachine(&cxx_path)?;
    let (stdlib, stdlib_version) = detect_stdlib(&cxx_path)?;

    // The SDK matters only for Apple targets; on those, the SDK version is
    // part of the ABI surface (availability markup, libc++ dylib on disk), so
    // it folds into the identity even for Homebrew GCC targeting darwin.
    let (sdk_path, sdk_version) = if target_triple.contains("apple") {
        detect_macos_sdk()
    } else {
        (None, None)
    };

    let cc = derive_cc(&cxx_path);
    let ar = derive_ar(&cxx_path, &compiler_id, &version);

    Ok(Toolchain {
        cxx: cxx_path,
        cc,
        ar,
        sdk_path,
        identity: ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id,
            version,
            target_triple,
            stdlib,
            stdlib_version,
            sdk_version,
        },
    })
}

/// Default toolchain: `c++` on PATH.
pub fn detect_default() -> Result<Toolchain> {
    detect("c++")
}

/// Classification of profile flags (CPPKG_TOML.md "Profiles and configs").
#[derive(Debug, Clone, Default)]
pub struct ClassifiedFlags {
    /// Propagate to dependency builds AND fold into their config hashes:
    /// -D_GLIBCXX_DEBUG, -D_GLIBCXX_ASSERTIONS, -D_GLIBCXX_USE_CXX11_ABI=*,
    /// -D_LIBCPP_HARDENING_MODE=*, -stdlib=*, -f*-abi* (extensible table).
    pub abi: Vec<String>,
    /// Consumer-only.
    pub consumer_only: Vec<String>,
    /// Subset of consumer_only that are -fsanitize=* (warning: deps are
    /// uninstrumented).
    pub sanitizers: Vec<String>,
}

pub fn classify_flags(flags: &[String]) -> ClassifiedFlags {
    let mut out = ClassifiedFlags::default();
    for flag in flags {
        if is_abi_flag(flag) {
            out.abi.push(flag.clone());
        } else {
            if flag.starts_with("-fsanitize=") {
                out.sanitizers.push(flag.clone());
            }
            out.consumer_only.push(flag.clone());
        }
    }
    out
}

fn is_abi_flag(flag: &str) -> bool {
    if flag == "-D_GLIBCXX_DEBUG"
        || flag == "-D_GLIBCXX_ASSERTIONS"
        || flag.starts_with("-D_GLIBCXX_USE_CXX11_ABI=")
        || flag.starts_with("-D_LIBCPP_HARDENING_MODE=")
        || flag.starts_with("-stdlib=")
    {
        return true;
    }
    // The -f*-abi* family: -fabi-version=N, -fc++-abi=..., -fclang-abi-compat=…
    // A substring match on "abi" after the -f prefix is deliberately broad —
    // misclassifying a hypothetical non-ABI -f...abi... flag as ABI-affecting
    // only causes an extra dependency rebuild, never a wrong reuse.
    if let Some(rest) = flag.strip_prefix("-f") {
        // -fsanitize=... is handled by the caller as consumer-only; nothing in
        // that family contains "abi", so no conflict here.
        if rest.contains("abi") {
            return true;
        }
    }
    false
}

/// Flag lowering for the GNU-like dialect (GCC, Clang, Apple Clang).
/// Typed requirements in, concrete argv fragments out; unlowerable input is
/// a hard error naming the requirement (never silently dropped).
pub trait Driver {
    /// e.g. (Cxx, 20) -> "-std=c++20" (strict; cxx-extensions reserved=false)
    fn std_flag(&self, lang: Lang, std: u32) -> Result<String>;
    /// -I<path> or -isystem <path>
    fn include_args(&self, path: &Path, system: bool) -> Vec<String>;
    /// -DKEY or -DKEY=VALUE
    fn define_arg(&self, key: &str, value: Option<&str>) -> String;
    /// -MD -MT <obj> -MF <depfile>
    fn depfile_args(&self, object: &Path, depfile: &Path) -> Vec<String>;
    /// -isysroot <sdk> when an SDK is present
    fn sysroot_args(&self, sdk: Option<&Path>) -> Vec<String>;
    /// -framework <name> (two argv entries)
    fn framework_args(&self, name: &str) -> Vec<String>;
    /// Config-default compile flags, mirroring CMake:
    /// Debug: -g | Release: -O3 -DNDEBUG | RelWithDebInfo: -O2 -g -DNDEBUG |
    /// MinSizeRel: -Os -DNDEBUG
    fn config_compile_flags(&self, config: crate::schema::BuildConfig) -> Vec<String>;
}

pub struct GnuDriver;

impl Driver for GnuDriver {
    fn std_flag(&self, lang: Lang, std: u32) -> Result<String> {
        // Validated against the closed sets GCC/Clang actually accept, so a
        // typo'd cxx-std fails here with a named requirement instead of as an
        // opaque compiler error mid-build.
        match lang {
            Lang::Cxx => match std {
                98 | 11 | 14 | 17 | 20 | 23 | 26 => Ok(format!("-std=c++{std:02}")),
                3 => Ok("-std=c++03".to_string()),
                _ => bail!("unsupported C++ standard `cxx-std = {std}` for the GNU dialect"),
            },
            Lang::C => match std {
                90 | 99 | 11 | 17 | 23 => Ok(format!("-std=c{std:02}")),
                89 => Ok("-std=c89".to_string()),
                _ => bail!("unsupported C standard `c-std = {std}` for the GNU dialect"),
            },
        }
    }
    fn include_args(&self, path: &Path, system: bool) -> Vec<String> {
        let p = path.to_string_lossy().into_owned();
        if system {
            // Two argv entries: ninja/compile_commands quoting stays trivial
            // and matches how CMake emits -isystem.
            vec!["-isystem".to_string(), p]
        } else {
            vec![format!("-I{p}")]
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
            "-MD".to_string(),
            "-MT".to_string(),
            object.to_string_lossy().into_owned(),
            "-MF".to_string(),
            depfile.to_string_lossy().into_owned(),
        ]
    }
    fn sysroot_args(&self, sdk: Option<&Path>) -> Vec<String> {
        match sdk {
            Some(p) => vec!["-isysroot".to_string(), p.to_string_lossy().into_owned()],
            None => vec![],
        }
    }
    fn framework_args(&self, name: &str) -> Vec<String> {
        vec!["-framework".to_string(), name.to_string()]
    }
    fn config_compile_flags(&self, config: crate::schema::BuildConfig) -> Vec<String> {
        use crate::schema::BuildConfig::*;
        let flags: &[&str] = match config {
            Debug => &["-g"],
            Release => &["-O3", "-DNDEBUG"],
            RelWithDebInfo => &["-O2", "-g", "-DNDEBUG"],
            MinSizeRel => &["-Os", "-DNDEBUG"],
        };
        flags.iter().map(|s| s.to_string()).collect()
    }
}

// ---------------------------------------------------------------------------
// Detection internals
// ---------------------------------------------------------------------------

/// Resolve a compiler argument: anything with a path separator is used as
/// given (must exist); a bare command name is searched on PATH.
fn resolve_command(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return p.is_file().then_some(p);
    }
    find_in_path(cmd)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run the compiler in preprocess-only macro-dump mode and parse
/// `#define NAME VALUE` lines. `source` empty means /dev/null input; otherwise
/// it is piped through stdin (used for the stdlib probe, which needs a real
/// #include to pull in the library's version macros).
fn macro_dump(cxx: &Path, source: &str) -> Result<HashMap<String, String>> {
    let mut cmd = Command::new(cxx);
    cmd.args(["-dM", "-E", "-x", "c++"]);
    if source.is_empty() {
        cmd.arg("/dev/null");
    } else {
        cmd.arg("-");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("failed to spawn {}", cxx.display()))?;
    if !source.is_empty() {
        // Best-effort write: if the compiler exits early we still want its
        // stderr, not a broken-pipe panic.
        let _ = child.stdin.take().expect("stdin piped").write_all(source.as_bytes());
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "`{} -dM -E -x c++` failed ({}): {}",
            cxx.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut macros = HashMap::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("#define ") {
            let (name, value) = match rest.split_once(' ') {
                Some((n, v)) => (n, v.trim()),
                None => (rest.trim(), ""),
            };
            // Function-like macros (NAME(args)) are irrelevant for identity.
            if !name.contains('(') {
                macros.insert(name.to_string(), value.to_string());
            }
        }
    }
    Ok(macros)
}

fn macro_value(macros: &HashMap<String, String>, name: &str) -> Result<String> {
    macros
        .get(name)
        .cloned()
        .with_context(|| format!("compiler did not define expected macro {name}"))
}

fn clang_version(macros: &HashMap<String, String>) -> Result<String> {
    Ok(format!(
        "{}.{}.{}",
        macro_value(macros, "__clang_major__")?,
        macro_value(macros, "__clang_minor__")?,
        macro_value(macros, "__clang_patchlevel__")?,
    ))
}

fn dumpmachine(cxx: &Path) -> Result<String> {
    let output = Command::new(cxx)
        .arg("-dumpmachine")
        .output()
        .with_context(|| format!("failed to run `{} -dumpmachine`", cxx.display()))?;
    if !output.status.success() {
        bail!("`{} -dumpmachine` failed ({})", cxx.display(), output.status);
    }
    let triple = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if triple.is_empty() {
        bail!("`{} -dumpmachine` produced no output", cxx.display());
    }
    Ok(triple)
}

/// Identify the default C++ standard library by macro-dumping a TU that
/// includes a library header: _LIBCPP_VERSION => libc++, __GLIBCXX__ =>
/// libstdc++. Neither macro is a compiler-predefined macro, hence the second
/// pass with a real #include. <version> is the canonical modern probe header;
/// <ciso646> is the pre-C++17 fallback spelling.
fn detect_stdlib(cxx: &Path) -> Result<(String, String)> {
    for header in ["version", "ciso646"] {
        let macros = match macro_dump(cxx, &format!("#include <{header}>\n")) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if let Some(v) = macros.get("_LIBCPP_VERSION") {
            return Ok(("libc++".to_string(), v.clone()));
        }
        if let Some(v) = macros.get("__GLIBCXX__") {
            return Ok(("libstdc++".to_string(), v.clone()));
        }
    }
    bail!(
        "could not detect the C++ standard library for `{}`: neither \
         _LIBCPP_VERSION nor __GLIBCXX__ defined after including <version>",
        cxx.display()
    )
}

fn detect_macos_sdk() -> (Option<PathBuf>, Option<String>) {
    let run = |arg: &str| -> Option<String> {
        let output = Command::new("xcrun").arg(arg).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let path = run("--show-sdk-path").map(PathBuf::from);
    // The SDK version comes from xcrun, not from the path: the path is a
    // stable `MacOSX.sdk` symlink whose contents change across Xcode updates.
    let version = run("--show-sdk-version");
    (path, version)
}

/// Derive the C driver from the C++ driver's file name:
///   clang++[-N] -> clang[-N] | g++[-N] -> gcc[-N] | c++ -> cc
/// A sibling in the compiler's own directory wins over PATH so that an
/// explicitly-pathed toolchain stays self-consistent. If no derived driver
/// exists anywhere, fall back to the C++ driver itself (GNU drivers compile C
/// fine when passed `-x c`).
fn derive_cc(cxx: &Path) -> PathBuf {
    let file_name = cxx.file_name().map(|n| n.to_string_lossy().into_owned());
    let derived = file_name.as_deref().and_then(|name| {
        if name.contains("clang++") {
            Some(name.replace("clang++", "clang"))
        } else if name.starts_with("g++") {
            Some(name.replacen("g++", "gcc", 1))
        } else if name.starts_with("c++") {
            Some(name.replacen("c++", "cc", 1))
        } else {
            None
        }
    });
    if let Some(name) = derived
        && let Some(found) = sibling_or_path(cxx, &name) {
            return found;
        }
    cxx.to_path_buf()
}

/// Pick the archiver matching the compiler family:
///   GNU N.x   -> gcc-ar-N (enables LTO-aware archives with Homebrew naming)
///   Clang-ish -> llvm-ar
/// preferred next to the compiler, then on PATH; final fallback is plain `ar`
/// on PATH (on macOS that is Apple's libtool-backed ar, fine for AppleClang).
fn derive_ar(cxx: &Path, compiler_id: &str, version: &str) -> PathBuf {
    let mut candidates: Vec<String> = Vec::new();
    if compiler_id == "GNU" {
        if let Some(major) = version.split('.').next() {
            candidates.push(format!("gcc-ar-{major}"));
        }
        candidates.push("gcc-ar".to_string());
    } else {
        candidates.push("llvm-ar".to_string());
    }
    for name in &candidates {
        if let Some(found) = sibling_or_path(cxx, name) {
            return found;
        }
    }
    find_in_path("ar").unwrap_or_else(|| PathBuf::from("ar"))
}

fn sibling_or_path(reference: &Path, name: &str) -> Option<PathBuf> {
    if let Some(dir) = reference.parent()
        && !dir.as_os_str().is_empty() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    find_in_path(name)
}

// ---------------------------------------------------------------------------
// Tests (run real compilers; this machine has Apple clang + Homebrew gcc)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::BuildConfig;

    /// Locate a Homebrew-style versioned g++ on PATH. The exact installed
    /// major version varies per machine, so probe a range instead of
    /// hardcoding one.
    fn find_homebrew_gxx() -> Option<String> {
        (10..=30)
            .rev()
            .map(|n| format!("g++-{n}"))
            .find(|name| find_in_path(name).is_some())
    }

    #[test]
    fn toolchain_detect_apple_clang() {
        let tc = detect("/usr/bin/c++").expect("detect /usr/bin/c++");
        assert_eq!(tc.identity.compiler_id, "AppleClang");
        assert_eq!(tc.identity.dialect, Dialect::Gnu);
        assert_eq!(tc.identity.stdlib, "libc++");
        assert!(!tc.identity.stdlib_version.is_empty());
        assert!(
            tc.identity.version.split('.').count() == 3,
            "version should be major.minor.patch, got {}",
            tc.identity.version
        );
        assert!(
            tc.identity.target_triple.contains("apple"),
            "unexpected triple {}",
            tc.identity.target_triple
        );
        assert!(tc.identity.sdk_version.is_some(), "macOS SDK version expected");
        assert!(tc.sdk_path.as_ref().is_some_and(|p| p.exists()));
        // /usr/bin/c++ -> /usr/bin/cc
        assert_eq!(tc.cc.file_name().unwrap(), "cc");
        assert!(tc.ar.is_file(), "ar not found: {}", tc.ar.display());
    }

    #[test]
    fn toolchain_detect_default_is_cxx_on_path() {
        let tc = detect_default().expect("detect default c++");
        assert!(matches!(tc.identity.compiler_id.as_str(), "AppleClang" | "Clang" | "GNU"));
    }

    #[test]
    fn toolchain_detect_homebrew_gnu() {
        let Some(gxx) = find_homebrew_gxx() else {
            eprintln!("SKIP: no Homebrew g++-N found on PATH");
            return;
        };
        let tc = detect(&gxx).expect("detect homebrew g++");
        assert_eq!(tc.identity.compiler_id, "GNU");
        assert_eq!(tc.identity.stdlib, "libstdc++");
        assert!(!tc.identity.stdlib_version.is_empty());
        let major = gxx.strip_prefix("g++-").unwrap();
        assert_eq!(tc.identity.version.split('.').next().unwrap(), major);
        assert_eq!(
            tc.cc.file_name().unwrap().to_string_lossy(),
            format!("gcc-{major}")
        );
        assert_eq!(
            tc.ar.file_name().unwrap().to_string_lossy(),
            format!("gcc-ar-{major}")
        );
    }

    #[test]
    fn toolchain_identities_differ_between_compilers() {
        let Some(gxx) = find_homebrew_gxx() else {
            eprintln!("SKIP: no Homebrew g++-N found on PATH");
            return;
        };
        let apple = detect("/usr/bin/c++").unwrap();
        let gnu = detect(&gxx).unwrap();
        assert_ne!(apple.identity.hash_input(), gnu.identity.hash_input());
    }

    #[test]
    fn toolchain_detect_missing_compiler_errors() {
        assert!(detect("definitely-not-a-compiler-xyz").is_err());
        assert!(detect("/nonexistent/path/c++").is_err());
    }

    #[test]
    fn toolchain_hash_input_is_stable_and_field_sensitive() {
        let id = ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id: "AppleClang".into(),
            version: "21.0.0".into(),
            target_triple: "arm64-apple-darwin25.5.0".into(),
            stdlib: "libc++".into(),
            stdlib_version: "210106".into(),
            sdk_version: Some("26.5".into()),
        };
        let expected = "cppkg-toolchain-identity-v1\n\
                        dialect=gnu\n\
                        compiler-id=AppleClang\n\
                        version=21.0.0\n\
                        target=arm64-apple-darwin25.5.0\n\
                        stdlib=libc++\n\
                        stdlib-version=210106\n\
                        sdk-version=26.5\n";
        assert_eq!(id.hash_input(), expected);

        let mut no_sdk = id.clone();
        no_sdk.sdk_version = None;
        assert!(no_sdk.hash_input().contains("sdk-version=none\n"));
        assert_ne!(no_sdk.hash_input(), id.hash_input());

        let mut other_version = id.clone();
        other_version.version = "21.0.1".into();
        assert_ne!(other_version.hash_input(), id.hash_input());
    }

    #[test]
    fn toolchain_classify_flags_table() {
        let flags: Vec<String> = [
            "-D_GLIBCXX_DEBUG",
            "-D_GLIBCXX_ASSERTIONS",
            "-D_GLIBCXX_USE_CXX11_ABI=0",
            "-D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_EXTENSIVE",
            "-stdlib=libc++",
            "-fabi-version=18",
            "-fc++-abi=itanium",
            "-fclang-abi-compat=17",
            "-fsanitize=address",
            "-fsanitize=undefined",
            "-O2",
            "-Wall",
            "-D_GLIBCXX_DEBUG_BACKTRACE_EXTRA", // prefix-alike, NOT exact match
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let c = classify_flags(&flags);
        assert_eq!(
            c.abi,
            vec![
                "-D_GLIBCXX_DEBUG",
                "-D_GLIBCXX_ASSERTIONS",
                "-D_GLIBCXX_USE_CXX11_ABI=0",
                "-D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_EXTENSIVE",
                "-stdlib=libc++",
                "-fabi-version=18",
                "-fc++-abi=itanium",
                "-fclang-abi-compat=17",
            ]
        );
        assert_eq!(c.sanitizers, vec!["-fsanitize=address", "-fsanitize=undefined"]);
        // Sanitizers stay in consumer_only too (they ARE consumer flags).
        assert_eq!(
            c.consumer_only,
            vec![
                "-fsanitize=address",
                "-fsanitize=undefined",
                "-O2",
                "-Wall",
                "-D_GLIBCXX_DEBUG_BACKTRACE_EXTRA",
            ]
        );
    }

    #[test]
    fn toolchain_classify_flags_empty() {
        let c = classify_flags(&[]);
        assert!(c.abi.is_empty() && c.consumer_only.is_empty() && c.sanitizers.is_empty());
    }

    #[test]
    fn toolchain_gnu_driver_std_flag() {
        let d = GnuDriver;
        assert_eq!(d.std_flag(Lang::Cxx, 20).unwrap(), "-std=c++20");
        assert_eq!(d.std_flag(Lang::Cxx, 11).unwrap(), "-std=c++11");
        assert_eq!(d.std_flag(Lang::Cxx, 98).unwrap(), "-std=c++98");
        assert_eq!(d.std_flag(Lang::Cxx, 3).unwrap(), "-std=c++03");
        assert_eq!(d.std_flag(Lang::C, 11).unwrap(), "-std=c11");
        assert_eq!(d.std_flag(Lang::C, 99).unwrap(), "-std=c99");
        assert_eq!(d.std_flag(Lang::C, 90).unwrap(), "-std=c90");
        // Unknown standards are hard errors, never passed through.
        assert!(d.std_flag(Lang::Cxx, 21).is_err());
        assert!(d.std_flag(Lang::C, 20).is_err());
    }

    #[test]
    fn toolchain_gnu_driver_args() {
        let d = GnuDriver;
        assert_eq!(d.include_args(Path::new("/inc"), false), vec!["-I/inc"]);
        assert_eq!(
            d.include_args(Path::new("/store/fmt/include"), true),
            vec!["-isystem", "/store/fmt/include"]
        );
        assert_eq!(d.define_arg("CORE_INTERNAL", None), "-DCORE_INTERNAL");
        assert_eq!(d.define_arg("CORE_API", Some("")), "-DCORE_API=");
        assert_eq!(d.define_arg("FOO", Some("bar")), "-DFOO=bar");
        assert_eq!(
            d.depfile_args(Path::new("obj/a.o"), Path::new("obj/a.o.d")),
            vec!["-MD", "-MT", "obj/a.o", "-MF", "obj/a.o.d"]
        );
        assert_eq!(
            d.sysroot_args(Some(Path::new("/SDK"))),
            vec!["-isysroot", "/SDK"]
        );
        assert!(d.sysroot_args(None).is_empty());
        assert_eq!(d.framework_args("CoreFoundation"), vec!["-framework", "CoreFoundation"]);
    }

    #[test]
    fn toolchain_gnu_driver_config_flags() {
        let d = GnuDriver;
        assert_eq!(d.config_compile_flags(BuildConfig::Debug), vec!["-g"]);
        assert_eq!(d.config_compile_flags(BuildConfig::Release), vec!["-O3", "-DNDEBUG"]);
        assert_eq!(
            d.config_compile_flags(BuildConfig::RelWithDebInfo),
            vec!["-O2", "-g", "-DNDEBUG"]
        );
        assert_eq!(
            d.config_compile_flags(BuildConfig::MinSizeRel),
            vec!["-Os", "-DNDEBUG"]
        );
    }

    /// End-to-end sanity: the flags the driver produces are accepted by the
    /// real detected compiler on a trivial TU.
    #[test]
    fn toolchain_driver_flags_accepted_by_real_compiler() {
        let tc = detect_default().expect("detect default");
        let d = GnuDriver;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("t.cpp");
        std::fs::write(&src, "#include <vector>\nint main(){return 0;}\n").unwrap();
        let obj = dir.path().join("t.o");
        let dep = dir.path().join("t.o.d");

        let mut cmd = Command::new(&tc.cxx);
        cmd.arg(d.std_flag(Lang::Cxx, 17).unwrap());
        cmd.args(d.include_args(dir.path(), true));
        cmd.arg(d.define_arg("CPPKG_TEST", Some("1")));
        cmd.args(d.config_compile_flags(BuildConfig::Release));
        cmd.args(d.depfile_args(&obj, &dep));
        cmd.args(d.sysroot_args(tc.sdk_path.as_deref()));
        cmd.args(["-c", "-o"]).arg(&obj).arg(&src);
        let out = cmd.output().expect("run compiler");
        assert!(
            out.status.success(),
            "compile failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(obj.is_file());
        assert!(dep.is_file(), "depfile not written");
    }
}
