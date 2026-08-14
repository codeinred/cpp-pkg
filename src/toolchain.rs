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

use std::path::{Path, PathBuf};

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
        todo!()
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
    let _ = cxx;
    todo!()
}

/// Default toolchain: `c++` on PATH.
pub fn detect_default() -> Result<Toolchain> {
    todo!()
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
    let _ = flags;
    todo!()
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
        let _ = (lang, std);
        todo!()
    }
    fn include_args(&self, path: &Path, system: bool) -> Vec<String> {
        let _ = (path, system);
        todo!()
    }
    fn define_arg(&self, key: &str, value: Option<&str>) -> String {
        let _ = (key, value);
        todo!()
    }
    fn depfile_args(&self, object: &Path, depfile: &Path) -> Vec<String> {
        let _ = (object, depfile);
        todo!()
    }
    fn sysroot_args(&self, sdk: Option<&Path>) -> Vec<String> {
        let _ = sdk;
        todo!()
    }
    fn framework_args(&self, name: &str) -> Vec<String> {
        let _ = name;
        todo!()
    }
    fn config_compile_flags(&self, config: crate::schema::BuildConfig) -> Vec<String> {
        let _ = config;
        todo!()
    }
}
