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

impl ToolchainIdentity {
    /// §2.1 cfg truth: what the current toolchain IS, on the two closed
    /// axes. os comes from the target triple (the target answers "what am
    /// I building *for*"): darwin => Macos; linux => Linux — gnu AND musl,
    /// the libc field is deliberately ignored; windows/mingw/msvc =>
    /// Windows. `clang` matches AppleClang (the googletest STREQUAL
    /// footgun); GNU => Gcc. An os outside the closed vocabulary is a hard
    /// error naming the triple — cfg must never silently evaluate against
    /// a platform the vocabulary cannot express.
    pub fn cfg_truth(&self) -> Result<crate::schema::CfgTruth> {
        use crate::schema::CfgAtom;
        let triple = self.target_triple.to_ascii_lowercase();
        let os = if triple.contains("darwin") || triple.contains("macos") {
            CfgAtom::Macos
        } else if triple.contains("linux") {
            CfgAtom::Linux
        } else if triple.contains("windows") || triple.contains("mingw") || triple.contains("msvc")
        {
            CfgAtom::Windows
        } else {
            bail!(
                "target triple `{}` has an OS outside the cfg vocabulary \
                 (windows, macos, linux); cannot evaluate cfg predicates \
                 for this toolchain",
                self.target_triple
            );
        };
        let compiler = match self.compiler_id.as_str() {
            // AppleClang IS clang for conditional purposes — upstream
            // manifests gate clang warning vocabulary, which Apple's build
            // accepts identically.
            "AppleClang" | "Clang" => CfgAtom::Clang,
            "GNU" => CfgAtom::Gcc,
            "MSVC" => CfgAtom::Msvc,
            other => bail!(
                "compiler id `{other}` has no cfg compiler atom \
                 (clang, gcc, msvc)"
            ),
        };
        Ok(crate::schema::CfgTruth { os, compiler })
    }
}

/// §5.4: Threads::Threads expansion — (compile flags, link flags). A pure
/// function of the §2.1 os axis, NOT the triple's libc field: glibc (any
/// vintage) and musl want `-pthread` equally; darwin needs nothing (libc
/// is pthreads); msvc needs nothing. Zero new hash inputs — the identity
/// containing the triple is already hashed.
pub fn threads_expansion(
    os: crate::schema::CfgAtom,
) -> (&'static [&'static str], &'static [&'static str]) {
    match os {
        crate::schema::CfgAtom::Linux => (&["-pthread"], &["-pthread"]),
        // Macos/Windows: empty. Non-os atoms are a caller error; expanding
        // to nothing is the safe answer (a missing -pthread is a link
        // error, never a silent miscompile).
        _ => (&[], &[]),
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

/// Propagation classes for the wave-1 fence (wave1-extensions.md §1.2).
/// The fence rejects only classes whose propagation through a public
/// bucket is categorically wrong; everything unknown fails open (`Other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagClass {
    /// The ABI table (extended): must live at [flags]/profile scope where
    /// it propagates into dependency builds and config hashes; an error at
    /// ANY target scope (§1.4).
    Abi,
    /// -fsanitize*: instrumentation the consumer chose; deps are
    /// uninstrumented, propagation would lie.
    Sanitizer,
    /// -W… (except the -Wl,/-Wa,/-Wp, transports) and -w: "warnings are
    /// private by nature; a library cannot volunteer its consumers into a
    /// diagnostic policy".
    Warning,
    /// -O*, -g, -g[0-9], -ggdb*, -glldb*: "optimization level is the
    /// consumer's (profile's) decision".
    OptDebug,
    /// Everything else, unknown included — the fence fails open.
    Other,
}

/// One classified payload word, tied back to the argv word it came from so
/// error messages can quote the user's spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedWord {
    /// Index of the originating argv word. Two-argv transport forms
    /// (-Xlinker <word>) attach the payload's classification to BOTH
    /// indices so callers can report either half of the pair.
    pub index: usize,
    /// The unwrapped payload actually classified (e.g. "-D_GLIBCXX_DEBUG"
    /// out of "-Wp,-D_GLIBCXX_DEBUG").
    pub payload: String,
    pub class: FlagClass,
}

/// Classify a flag list, unwrapping driver pass-through (transport)
/// spellings BEFORE matching — the §1.2 laundering fix. `-Wl,`/`-Wa,`/
/// `-Wp,` prefixes are stripped and their comma-separated payload words
/// classified individually; the two-argv `-Xlinker`/`-Xpreprocessor`/
/// `-Xassembler <word>` forms classify the following word. Transport is
/// never itself Warning class — it is transport, not a warning — but it
/// never launders ABI or sanitizer payloads past the fence either
/// (`-Wp,-D_GLIBCXX_DEBUG` classifies Abi; `-Wl,-framework,X` stays
/// Other).
pub fn classify_word_sequence(flags: &[String]) -> Vec<ClassifiedWord> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        let word = &flags[i];
        if is_two_argv_transport(word) {
            if let Some(payload_word) = flags.get(i + 1) {
                // The payload's classification attaches to both argv
                // indices: schema errors can then point at whichever word
                // the user will recognize.
                let classified = unwrap_and_classify(payload_word);
                for (payload, class) in &classified {
                    out.push(ClassifiedWord { index: i, payload: payload.clone(), class: *class });
                }
                for (payload, class) in classified {
                    out.push(ClassifiedWord { index: i + 1, payload, class });
                }
                i += 2;
                continue;
            }
            // Dangling transport (nothing follows): nothing to classify;
            // the transport word itself fails open.
            out.push(ClassifiedWord { index: i, payload: word.clone(), class: FlagClass::Other });
            i += 1;
            continue;
        }
        for (payload, class) in unwrap_and_classify(word) {
            out.push(ClassifiedWord { index: i, payload, class });
        }
        i += 1;
    }
    out
}

fn is_two_argv_transport(word: &str) -> bool {
    matches!(word, "-Xlinker" | "-Xpreprocessor" | "-Xassembler")
}

/// Strip a comma-transport prefix (if any) and classify each payload word;
/// a plain word classifies as itself. Payload words cannot contain commas
/// (the split consumed them), so one unwrap level suffices.
fn unwrap_and_classify(word: &str) -> Vec<(String, FlagClass)> {
    for prefix in ["-Wl,", "-Wa,", "-Wp,"] {
        if let Some(rest) = word.strip_prefix(prefix) {
            return rest
                .split(',')
                .map(|part| (part.to_string(), classify_plain_word(part)))
                .collect();
        }
    }
    vec![(word.to_string(), classify_plain_word(word))]
}

/// The class table for a single, already-unwrapped argv word. ABI first to
/// keep v0 `classify_flags` semantics bit-identical (no member of the ABI
/// table overlaps -fsanitize*, so the order only matters in principle).
fn classify_plain_word(word: &str) -> FlagClass {
    if is_abi_flag(word) {
        return FlagClass::Abi;
    }
    if word.starts_with("-fsanitize") {
        return FlagClass::Sanitizer;
    }
    // -Wl,/-Wa,/-Wp, spellings never reach here (unwrapped above), so any
    // remaining -W… word is a real diagnostic flag.
    if word == "-w" || word.starts_with("-W") {
        return FlagClass::Warning;
    }
    if word.starts_with("-O") || word.starts_with("-ggdb") || word.starts_with("-glldb") {
        return FlagClass::OptDebug;
    }
    if let Some(rest) = word.strip_prefix("-g") {
        // Exactly -g or -g<digit>; -gdwarf-5 & friends are format
        // selectors, not levels — they fail open per the spec table.
        if rest.is_empty() || (rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit()) {
            return FlagClass::OptDebug;
        }
    }
    FlagClass::Other
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
    /// Subset of consumer_only that are -fsanitize* (warning: deps are
    /// uninstrumented).
    pub sanitizers: Vec<String>,
}

/// v0 profile-scope split, reimplemented on `classify_word_sequence` so the
/// ABI table exists exactly once. New over v0: transported ABI payloads
/// (`-Wp,-D_GLIBCXX_DEBUG`, `-Xpreprocessor -D_GLIBCXX_DEBUG`) now land in
/// the abi bucket — transport must not launder ABI past the config hash.
/// Plain-word inputs classify bit-identically to v0 (no store keys move).
pub fn classify_flags(flags: &[String]) -> ClassifiedFlags {
    let words = classify_word_sequence(flags);
    let mut has_abi = vec![false; flags.len()];
    let mut has_san = vec![false; flags.len()];
    for w in &words {
        match w.class {
            FlagClass::Abi => has_abi[w.index] = true,
            FlagClass::Sanitizer => has_san[w.index] = true,
            _ => {}
        }
    }
    // Two-argv pairs carry the payload class on both indices, so both
    // halves of "-Xpreprocessor -D_GLIBCXX_DEBUG" travel to the abi bucket
    // together, in argv order.
    let mut out = ClassifiedFlags::default();
    for (i, flag) in flags.iter().enumerate() {
        if has_abi[i] {
            out.abi.push(flag.clone());
        } else {
            if has_san[i] {
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

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// One golden per §1.2 table row, plain-word spellings.
    #[test]
    fn toolchain_classify_word_sequence_plain_rows() {
        use FlagClass::*;
        let cases: &[(&str, FlagClass)] = &[
            // ABI table
            ("-D_GLIBCXX_DEBUG", Abi),
            ("-D_GLIBCXX_ASSERTIONS", Abi),
            ("-D_GLIBCXX_USE_CXX11_ABI=0", Abi),
            ("-D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_FAST", Abi),
            ("-stdlib=libc++", Abi),
            ("-fabi-version=18", Abi),
            ("-fclang-abi-compat=17", Abi),
            // sanitizer (whole -fsanitize* family, not just -fsanitize=)
            ("-fsanitize=address", Sanitizer),
            ("-fsanitize-recover=all", Sanitizer),
            ("-fsanitize-address-use-after-scope", Sanitizer),
            // warning
            ("-Wall", Warning),
            ("-Werror", Warning),
            ("-Wno-deprecated", Warning),
            ("-w", Warning),
            ("-Wthread-safety", Warning),
            // opt/debug
            ("-O0", OptDebug),
            ("-O2", OptDebug),
            ("-O", OptDebug),
            ("-Os", OptDebug),
            ("-Ofast", OptDebug),
            ("-g", OptDebug),
            ("-g0", OptDebug),
            ("-g3", OptDebug),
            ("-ggdb", OptDebug),
            ("-ggdb3", OptDebug),
            ("-glldb", OptDebug),
            // fail open
            ("-fno-exceptions", Other),
            ("-pthread", Other),
            ("-mavx2", Other),
            ("-framework", Other),
            ("-lrt", Other),
            ("-gdwarf-5", Other),   // format selector, not a debug level
            ("-D_GLIBCXX_DEBUG_BACKTRACE_EXTRA", Other), // alike, NOT exact
            ("--totally-unknown", Other),
        ];
        for (flag, expected) in cases {
            let got = classify_word_sequence(&s(&[flag]));
            assert_eq!(
                got,
                vec![ClassifiedWord { index: 0, payload: flag.to_string(), class: *expected }],
                "flag {flag}"
            );
        }
    }

    /// §1.2 laundering fix: comma transports are unwrapped and every
    /// payload word classified individually; transport is never Warning.
    #[test]
    fn toolchain_classify_word_sequence_comma_transports() {
        use FlagClass::*;
        // The canonical laundering attempt: an ABI define smuggled through
        // the preprocessor transport.
        let got = classify_word_sequence(&s(&["-Wp,-D_GLIBCXX_DEBUG"]));
        assert_eq!(
            got,
            vec![ClassifiedWord { index: 0, payload: "-D_GLIBCXX_DEBUG".into(), class: Abi }]
        );

        // Benign linker transport still passes: -framework is in no
        // rejected class, and the -Wl, spelling is NOT Warning.
        let got = classify_word_sequence(&s(&["-Wl,-framework,CoreFoundation"]));
        assert_eq!(
            got,
            vec![
                ClassifiedWord { index: 0, payload: "-framework".into(), class: Other },
                ClassifiedWord { index: 0, payload: "CoreFoundation".into(), class: Other },
            ]
        );

        // Sanitizer through the linker transport is caught too.
        let got = classify_word_sequence(&s(&["-Wl,-fsanitize=address"]));
        assert_eq!(got[0].class, Sanitizer);

        // Assembler transport with a debug-level payload.
        let got = classify_word_sequence(&s(&["-Wa,-g"]));
        assert_eq!(
            got,
            vec![ClassifiedWord { index: 0, payload: "-g".into(), class: OptDebug }]
        );

        // Multi-word payloads keep per-word classes under one index.
        let got = classify_word_sequence(&s(&["-Wl,--as-needed,-lrt"]));
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|w| w.index == 0 && w.class == Other));
    }

    /// Two-argv transports classify the following word and attach the
    /// class to BOTH indices (schema reports the user's spelling).
    #[test]
    fn toolchain_classify_word_sequence_two_argv_transports() {
        use FlagClass::*;
        let got = classify_word_sequence(&s(&["-Xpreprocessor", "-D_GLIBCXX_DEBUG", "-O2"]));
        assert_eq!(
            got,
            vec![
                ClassifiedWord { index: 0, payload: "-D_GLIBCXX_DEBUG".into(), class: Abi },
                ClassifiedWord { index: 1, payload: "-D_GLIBCXX_DEBUG".into(), class: Abi },
                ClassifiedWord { index: 2, payload: "-O2".into(), class: OptDebug },
            ]
        );

        let got = classify_word_sequence(&s(&["-Xlinker", "-lfoo"]));
        assert!(got.iter().all(|w| w.class == Other));
        assert_eq!((got[0].index, got[1].index), (0, 1));

        // -Xlinker payload that is itself a comma transport unwraps too.
        let got = classify_word_sequence(&s(&["-Xlinker", "-Wl,-fsanitize=address"]));
        assert!(got.iter().any(|w| w.class == Sanitizer));

        // Dangling transport at end of list fails open, consumes one word.
        let got = classify_word_sequence(&s(&["-Xlinker"]));
        assert_eq!(
            got,
            vec![ClassifiedWord { index: 0, payload: "-Xlinker".into(), class: Other }]
        );
    }

    /// classify_flags (profile scope) no longer lets transport launder ABI
    /// or sanitizer payloads; plain words split exactly as in v0.
    #[test]
    fn toolchain_classify_flags_unwraps_transports() {
        let c = classify_flags(&s(&[
            "-Wp,-D_GLIBCXX_DEBUG",         // transported ABI -> abi bucket
            "-Xpreprocessor",               // pair travels together...
            "-D_GLIBCXX_ASSERTIONS",        // ...into the abi bucket
            "-Wl,-framework,CoreFoundation", // benign -> consumer_only
            "-Xlinker",                     // pair with sanitizer payload
            "-fsanitize=thread",
        ]));
        assert_eq!(
            c.abi,
            s(&["-Wp,-D_GLIBCXX_DEBUG", "-Xpreprocessor", "-D_GLIBCXX_ASSERTIONS"])
        );
        assert_eq!(
            c.consumer_only,
            s(&["-Wl,-framework,CoreFoundation", "-Xlinker", "-fsanitize=thread"])
        );
        assert_eq!(c.sanitizers, s(&["-Xlinker", "-fsanitize=thread"]));
    }

    fn identity(compiler_id: &str, triple: &str) -> ToolchainIdentity {
        ToolchainIdentity {
            dialect: Dialect::Gnu,
            compiler_id: compiler_id.into(),
            version: "1.0.0".into(),
            target_triple: triple.into(),
            stdlib: "libc++".into(),
            stdlib_version: "1".into(),
            sdk_version: None,
        }
    }

    #[test]
    fn toolchain_cfg_truth_table() {
        use crate::schema::CfgAtom;
        // AppleClang => Clang (the STREQUAL footgun, §2.1).
        let t = identity("AppleClang", "arm64-apple-darwin25.5.0").cfg_truth().unwrap();
        assert!(matches!(t.os, CfgAtom::Macos));
        assert!(matches!(t.compiler, CfgAtom::Clang));

        // glibc and musl are both Linux — the libc field is ignored.
        let t = identity("GNU", "x86_64-pc-linux-gnu").cfg_truth().unwrap();
        assert!(matches!(t.os, CfgAtom::Linux));
        assert!(matches!(t.compiler, CfgAtom::Gcc));
        let t = identity("Clang", "x86_64-alpine-linux-musl").cfg_truth().unwrap();
        assert!(matches!(t.os, CfgAtom::Linux));
        assert!(matches!(t.compiler, CfgAtom::Clang));

        let t = identity("GNU", "x86_64-w64-mingw32").cfg_truth().unwrap();
        assert!(matches!(t.os, CfgAtom::Windows));

        // Unknown os: hard error naming the triple.
        let err = identity("Clang", "wasm32-unknown-wasi").cfg_truth().unwrap_err();
        assert!(err.to_string().contains("wasm32-unknown-wasi"), "{err}");
        // Unknown compiler id: hard error.
        assert!(identity("TurboC", "x86_64-pc-linux-gnu").cfg_truth().is_err());
    }

    #[test]
    fn toolchain_threads_expansion_table() {
        use crate::schema::CfgAtom;
        assert_eq!(
            threads_expansion(CfgAtom::Linux),
            (&["-pthread"][..], &["-pthread"][..])
        );
        assert_eq!(threads_expansion(CfgAtom::Macos), (&[][..], &[][..]));
        assert_eq!(threads_expansion(CfgAtom::Windows), (&[][..], &[][..]));
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
