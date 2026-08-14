//! `CppPkg.toml` types + parsing + validation. Normative spec: CPPKG_TOML.md
//! (v0 core + wave-1 fold-in; binding wave-1 semantics in
//! docs/design/wave1-extensions.md).
//!
//! Implementation notes (contract):
//! - kebab-case TOML keys (`schema-version`, `cxx-std`, `exposes-namespace`).
//! - `VisibilitySplit` deserializes from EITHER a bare array (=> all private;
//!   sugar applies uniformly to includes/defines/dependencies and, since
//!   wave 1, the three target flag keys) OR a table
//!   `{ public = [...], private = [...] }`.
//! - `DependencySpec` source: exactly one of git(+tag|rev), url(+sha256), or
//!   `system = true`; anything else is a validation error.
//! - Validation (all hard errors, with actionable messages):
//!   * charset `[a-zA-Z0-9_-]+` for package name, dependency keys, target
//!     names, generate-step names, export names
//!   * `needs` entries must be dependency keys (one namespace across
//!     [dependencies] and [dev-dependencies]); `needs` cycles are errors
//!   * profile names must be one of the four built-ins (v0)
//!   * ABI-affecting profile flags are ALLOWED (they propagate to deps,
//!     see toolchain::classify_flags); `-fsanitize=*` triggers a warning
//!     (returned in `Warnings`, printed by the CLI)
//!   * per-target flags pass the propagation fence (spec §1.2): public
//!     buckets reject ABI/sanitizer/warning/opt-debug classes; ABI-classified
//!     words are rejected at ANY target scope; unknown flags fail open
//!   * cfg sub-tables use the closed v1 atom vocabulary; combinators and the
//!     other reserved spellings each get their distinct error
//!   * `${...}` interpolation is only accepted in the whitelisted positions
//!     (spec §0.3); resolution itself happens later, in `crate::interp`
//!   * unknown TOML keys should be rejected (serde deny_unknown_fields)
//! - `[target-defaults]` is merged into eligible targets AT LOAD, before
//!   validation, so errors point at effective values (spec §7.2). The raw
//!   table is retained in `ProjectFile::target_defaults_raw` for `--query`.
//!
//! Parsing strategy: serde deserializes into private `Raw*` structs (which
//! own all the TOML-shape concerns: kebab-case, deny_unknown_fields, the
//! bare-array-or-table sugar), and an explicit validation pass converts them
//! into the serde-free public types. This keeps every validation rule in one
//! auditable place and lets error messages name the offending key. Reserved
//! spellings that need a *better* error than serde's generic unknown-key
//! message are declared as real fields (or caught in manual table walks) so
//! the message can say "reserved" and name the fix.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde::de::{MapAccess, Visitor};

use crate::Result;
use crate::interp;

/// The four CMake-compatible build configurations (v0 profiles; custom
/// profiles with `base-config` are reserved, not implemented).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildConfig {
    Debug,
    Release,
    RelWithDebInfo,
    MinSizeRel,
}

impl BuildConfig {
    /// CMake spelling: "Debug", "Release", "RelWithDebInfo", "MinSizeRel".
    pub fn cmake_name(self) -> &'static str {
        match self {
            BuildConfig::Debug => "Debug",
            BuildConfig::Release => "Release",
            BuildConfig::RelWithDebInfo => "RelWithDebInfo",
            BuildConfig::MinSizeRel => "MinSizeRel",
        }
    }
    /// TOML/CLI spelling: "debug", "release", "relwithdebinfo", "minsizerel".
    pub fn key(self) -> &'static str {
        match self {
            BuildConfig::Debug => "debug",
            BuildConfig::Release => "release",
            BuildConfig::RelWithDebInfo => "relwithdebinfo",
            BuildConfig::MinSizeRel => "minsizerel",
        }
    }
    pub fn from_key(key: &str) -> Result<Self> {
        match key {
            "debug" => Ok(BuildConfig::Debug),
            "release" => Ok(BuildConfig::Release),
            "relwithdebinfo" => Ok(BuildConfig::RelWithDebInfo),
            "minsizerel" => Ok(BuildConfig::MinSizeRel),
            other => bail!(
                "unknown build config '{other}' \
                 (expected one of: debug, release, relwithdebinfo, minsizerel)"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// cfg predicates (wave 1, spec §2.1)
// ---------------------------------------------------------------------------

/// Closed v1 predicate vocabulary. Combinators (`all(...)`, `any(...)`,
/// `not(...)`, version comparisons) and `apple-clang` are reserved with
/// distinct errors at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CfgAtom {
    Windows,
    Macos,
    Linux,
    Unix,
    Clang,
    Gcc,
    Msvc,
}

impl CfgAtom {
    pub fn key(self) -> &'static str {
        match self {
            CfgAtom::Windows => "windows",
            CfgAtom::Macos => "macos",
            CfgAtom::Linux => "linux",
            CfgAtom::Unix => "unix",
            CfgAtom::Clang => "clang",
            CfgAtom::Gcc => "gcc",
            CfgAtom::Msvc => "msvc",
        }
    }
    fn from_key(key: &str) -> Option<CfgAtom> {
        Some(match key {
            "windows" => CfgAtom::Windows,
            "macos" => CfgAtom::Macos,
            "linux" => CfgAtom::Linux,
            "unix" => CfgAtom::Unix,
            "clang" => CfgAtom::Clang,
            "gcc" => CfgAtom::Gcc,
            "msvc" => CfgAtom::Msvc,
            _ => return None,
        })
    }
}

/// A v1 predicate is a single atom (parsed from a `cfg.<atom>` sub-table
/// key). Evaluated at plan time against the toolchain-derived truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfgPredicate {
    pub atom: CfgAtom,
}

impl fmt::Display for CfgPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.atom.key())
    }
}

/// What the current toolchain IS — derived by `toolchain.rs` from
/// `ToolchainIdentity` (os from the target triple; `clang` matches
/// AppleClang). `os` is one of Windows|Macos|Linux; `compiler` is one of
/// Clang|Gcc|Msvc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfgTruth {
    pub os: CfgAtom,
    pub compiler: CfgAtom,
}

impl CfgPredicate {
    /// `unix` is true iff os is macos or linux (future unixes join
    /// additively). Pure; evaluated at plan time (spec §2.2).
    pub fn eval(&self, truth: &CfgTruth) -> bool {
        match self.atom {
            CfgAtom::Unix => matches!(truth.os, CfgAtom::Macos | CfgAtom::Linux),
            CfgAtom::Windows | CfgAtom::Macos | CfgAtom::Linux => truth.os == self.atom,
            CfgAtom::Clang | CfgAtom::Gcc | CfgAtom::Msvc => truth.compiler == self.atom,
        }
    }
}

// ---------------------------------------------------------------------------
// Public manifest types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub package: PackageMeta,
    pub toolchains: BTreeMap<String, ToolchainPreset>,
    pub profiles: BTreeMap<String, Profile>,
    /// `[flags]` (wave 1, spec §1.1); empty when absent.
    pub flags: PackageFlags,
    pub dependencies: BTreeMap<String, DependencySpec>,
    /// `[dev-dependencies]` (wave 1, §3.2). One namespace with
    /// `dependencies`: key collision across the tables is a load error.
    pub dev_dependencies: BTreeMap<String, DependencySpec>,
    /// `[generate.<name>]` steps (wave 1, §4.1), keyed by step name.
    pub generate: BTreeMap<String, GenerateStep>,
    /// `[export]` (wave 1, §6.1); defaults filled from `package.name`.
    pub export: ExportMeta,
    pub targets: BTreeMap<String, TargetSpec>,
    /// `[target-defaults]` is APPLIED at load (§7.2: before validation, so
    /// errors point at effective values); the raw table is retained only for
    /// `--query` display.
    pub target_defaults_raw: Option<toml::Table>,
}

#[derive(Debug, Clone)]
pub struct PackageMeta {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolchainPreset {
    pub cxx: String,
    /// Derived from `cxx` at detection time if absent.
    pub cc: Option<String>,
    pub ar: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub cxx_flags: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

/// `[flags]` — every target, every profile (wave 1, §1.1). Hoisted-profile
/// semantics: no visibility split (it is environment, not interface).
/// ABI-classified entries reach dependency builds + hashes via the existing
/// profile-ABI machinery; everything else is consumer-targets-only.
#[derive(Debug, Clone, Default)]
pub struct PackageFlags {
    pub cxx_flags: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
    /// `[flags.cfg.<pred>]` groups, in document order.
    pub cfg: Vec<(CfgPredicate, PackageFlagsGroup)>,
}

#[derive(Debug, Clone, Default)]
pub struct PackageFlagsGroup {
    pub cxx_flags: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

/// `[export]` (wave 1, §6.1). Both fields default to `package.name`.
#[derive(Debug, Clone)]
pub struct ExportMeta {
    /// `find_package(<cmake-name>)` / `<CmakeName>Config.cmake` name.
    pub cmake_name: String,
    /// IMPORTED-target namespace (`<namespace>::<target>`).
    pub namespace: String,
}

#[derive(Debug, Clone)]
pub enum GitRef {
    Tag(String),
    Rev(String),
}

#[derive(Debug, Clone)]
pub enum SourceSpec {
    Git { url: String, reference: GitRef },
    Url { url: String, sha256: String },
    /// `system = true` (wave 1, §5.3): resolve from the machine, never
    /// build. The lockfile records the declaration; machine facts live in
    /// the sysdep store entry.
    System { min_version: Option<String> },
}

/// `exposes-targets`: list form claims ownership; map form also renames
/// (extracted name -> exposed name).
#[derive(Debug, Clone, Default)]
pub struct ExposesTargets {
    pub claims: Vec<String>,
    pub renames: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DependencySpec {
    pub source: SourceSpec,
    /// CMake cache options. Hashed as LITERAL strings — never normalized.
    pub options: BTreeMap<String, String>,
    /// find_dependency edges; drives build order + CMAKE_PREFIX_PATH closure.
    pub needs: Vec<String>,
    /// `find_package(<this>)` name used by the probe; defaults to the dep key.
    pub find_package: Option<String>,
    pub exposes_namespace: Vec<String>,
    pub exposes_targets: ExposesTargets,
    /// Wave 1 (§5.2): patch files, manifest-dir-relative, application order.
    /// Duplicates are a load error; illegal on `system = true`.
    pub patches: Vec<PathBuf>,
    /// Wave 1 (tool-fix A.5): configure root below the checkout
    /// (`<checkout>/<subdir>`). Non-empty, relative, no `..`.
    pub subdir: Option<String>,
    /// Wave 1 (§1.1): opt this dep's public headers out of `-isystem`
    /// (`Some(false)` => back to `-I`). `None` => dependency default (true).
    pub system_includes: Option<bool>,
    /// Wave 1 (§2.2): `Some(pred)` iff declared under
    /// `[cfg.<pred>.dependencies.*]` (or `...dev-dependencies...`). The same
    /// key declared twice (any two branches, or branch + unconditional) is a
    /// hard error.
    pub cfg: Option<CfgPredicate>,
    /// Wave 1 (§3.2): true iff declared in a dev table (set by the loader;
    /// both tables share this one struct).
    pub dev: bool,
}

impl DependencySpec {
    /// A spec with the given source and every other field defaulted.
    /// Convenience for tests and programmatic construction.
    pub fn from_source(source: SourceSpec) -> Self {
        DependencySpec {
            source,
            options: BTreeMap::new(),
            needs: Vec::new(),
            find_package: None,
            exposes_namespace: Vec::new(),
            exposes_targets: ExposesTargets::default(),
            patches: Vec::new(),
            subdir: None,
            system_includes: None,
            cfg: None,
            dev: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Executable,
    StaticLibrary,
}

/// public/private lists; bare array deserializes as all-private.
#[derive(Debug, Clone, Default)]
pub struct VisibilitySplit {
    pub public: Vec<String>,
    pub private: Vec<String>,
}

/// `public-headers` (wave 1, §6.4): a TOTAL override of header derivation —
/// never merged, not cfg-conditionable. `${gen}` is not legal in `base`.
#[derive(Debug, Clone)]
pub struct PublicHeaders {
    pub base: String,
    pub patterns: Vec<String>,
}

/// One `runtime-data` entry (wave 1, §6.5). `patterns` defaults to
/// `["**/*"]`; `to` defaults to the last path component of `from`.
#[derive(Debug, Clone)]
pub struct RuntimeData {
    pub from: String,
    pub patterns: Vec<String>,
    pub to: String,
}

/// One `[[targets.<t>.run]]` invocation (wave 1, §3.2). Legal only on
/// `test = true` targets; zero entries = one default invocation.
#[derive(Debug, Clone, Default)]
pub struct RunEntry {
    /// Optional, unique per target.
    pub name: Option<String>,
    pub args: Vec<String>,
    /// Relative to the project root.
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub env_remove: Vec<String>,
    /// Pass iff nonzero exit or signal death.
    pub expect_failure: bool,
}

/// Cfg-conditionable target keys (wave 1, §2.2): list-valued keys only.
/// Scalars, markers, `run`, and `public-headers` in a cfg group each get
/// their distinct hard error at load.
#[derive(Debug, Clone, Default)]
pub struct TargetCfgGroup {
    pub sources: Vec<String>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
    pub cxx_flags: VisibilitySplit,
    pub c_flags: VisibilitySplit,
    pub link_flags: VisibilitySplit,
    pub runtime_data: Vec<RuntimeData>,
}

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub kind: TargetKind,
    /// Glob patterns; expansion (sorted byte order, `!` negation per spec
    /// §0.4) happens in graph::plan.
    pub sources: Vec<String>,
    pub cxx_std: Option<u32>,
    pub c_std: Option<u32>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
    /// Wave 1 (§1.1): per-target flags; fence-checked at load.
    pub cxx_flags: VisibilitySplit,
    pub c_flags: VisibilitySplit,
    pub link_flags: VisibilitySplit,
    /// Wave 1 (§1.1). `None` => kind-dependent default at use site
    /// (project targets: false — consumers see `-I`).
    pub system_includes: Option<bool>,
    /// Wave 1 (§3.2): dev-graph membership.
    pub dev: bool,
    /// Wave 1 (§3.2): runner registration; implies `dev`; executables only.
    pub test: bool,
    /// Wave 1 (§6.1): export this target. Default false; may be filled by
    /// `[target-defaults]`, eligibility-gated (skips dev/test targets).
    pub install: bool,
    /// Wave 1 (§6.4): total override of header derivation.
    pub public_headers: Option<PublicHeaders>,
    /// Wave 1 (§6.5).
    pub runtime_data: Vec<RuntimeData>,
    /// Wave 1 (§3.2): legal only when `test = true`.
    pub run: Vec<RunEntry>,
    /// `[targets.<t>.cfg.<pred>]` groups, in document order (§2.2).
    pub cfg: Vec<(CfgPredicate, TargetCfgGroup)>,
}

impl Default for TargetSpec {
    /// Test-construction convenience for sibling modules: an empty
    /// executable. `kind` has no semantically neutral default — real
    /// construction goes through parsing, which always states the kind.
    fn default() -> Self {
        TargetSpec {
            kind: TargetKind::Executable,
            sources: Vec::new(),
            cxx_std: None,
            c_std: None,
            includes: VisibilitySplit::default(),
            defines: VisibilitySplit::default(),
            dependencies: VisibilitySplit::default(),
            cxx_flags: VisibilitySplit::default(),
            c_flags: VisibilitySplit::default(),
            link_flags: VisibilitySplit::default(),
            system_includes: None,
            dev: false,
            test: false,
            install: false,
            public_headers: None,
            runtime_data: Vec::new(),
            run: Vec::new(),
            cfg: Vec::new(),
        }
    }
}

/// One `[generate.<name>]` step (wave 1, §4.1).
#[derive(Debug, Clone)]
pub struct GenerateStep {
    pub name: String,
    /// Exactly one of template/command (validated at load).
    pub action: GenerateAction,
    /// Declared extra inputs (project-root-relative). Existence is checked
    /// at plan time, for activated steps only (§4.2).
    pub inputs: Vec<String>,
    /// checked-in mode: the committed source-tree path `cpp-pkg gen`
    /// refreshes. Steps with this set live OUTSIDE the build graph.
    pub checked_in: Option<String>,
}

#[derive(Debug, Clone)]
pub enum GenerateAction {
    /// `@VAR@` substitution (`@ONLY` parity). `output` is `${gen}`-relative.
    Template {
        template: String,
        output: String,
        vars: BTreeMap<String, String>,
    },
    /// argv (no shell), sandboxed. `stdout` is `${gen}`-relative.
    Command {
        argv: Vec<String>,
        stdin: Option<String>,
        stdout: String,
    },
}

impl GenerateAction {
    /// The `${gen}`-relative output path of this step.
    pub fn output(&self) -> &str {
        match self {
            GenerateAction::Template { output, .. } => output,
            GenerateAction::Command { stdout, .. } => stdout,
        }
    }
}

/// Non-fatal findings surfaced to the user by the CLI (e.g. sanitizer flags
/// present: dependencies are uninstrumented).
#[derive(Debug, Clone, Default)]
pub struct Warnings(pub Vec<String>);

// ---------------------------------------------------------------------------
// Raw (serde) layer — TOML shape only, no validation beyond structure.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawProject {
    schema_version: u32,
    package: RawPackage,
    #[serde(default)]
    export: Option<RawExport>,
    #[serde(default)]
    toolchains: BTreeMap<String, RawToolchain>,
    #[serde(default)]
    profiles: BTreeMap<String, RawProfile>,
    #[serde(default)]
    flags: Option<RawFlags>,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, RawDependency>,
    /// `[cfg.<pred>.…]` package-scope conditional tables.
    #[serde(default)]
    cfg: BTreeMap<String, RawCfgScope>,
    #[serde(default)]
    generate: BTreeMap<String, RawGenerate>,
    #[serde(default)]
    target_defaults: Option<toml::Table>,
    #[serde(default)]
    targets: BTreeMap<String, RawTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawExport {
    cmake_name: Option<String>,
    namespace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawToolchain {
    cxx: String,
    cc: Option<String>,
    ar: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
struct RawProfile {
    cxx_flags: Vec<String>,
    c_flags: Vec<String>,
    link_flags: Vec<String>,
    /// Reserved position ([profiles.*.cfg.*]); declared so the error can
    /// say so instead of serde's generic unknown-key message.
    cfg: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
struct RawFlags {
    cxx_flags: Vec<String>,
    c_flags: Vec<String>,
    link_flags: Vec<String>,
    cfg: Option<OrderedTable<toml::Value>>,
}

/// `[cfg.<pred>]` package scope. `targets`/`generate` are reserved
/// positions; `flags` is a misplacement (the real spelling is
/// `[flags.cfg.<pred>]`); anything else is unknown. Declared explicitly so
/// each gets its distinct error.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawCfgScope {
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, RawDependency>,
    targets: Option<toml::Value>,
    generate: Option<toml::Value>,
    flags: Option<toml::Value>,
    #[serde(flatten)]
    other: BTreeMap<String, toml::Value>,
}

// Source fields are flat Options rather than a serde enum so that malformed
// combinations (git+url, git without a ref, url without sha256, system plus
// either, ...) reach the validation pass and get a message naming the
// dependency — an untagged enum would collapse them all into one unhelpful
// "no variant matched".
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawDependency {
    git: Option<String>,
    tag: Option<String>,
    rev: Option<String>,
    url: Option<String>,
    sha256: Option<String>,
    system: Option<bool>,
    min_version: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, String>,
    #[serde(default)]
    needs: Vec<String>,
    find_package: Option<String>,
    #[serde(default)]
    exposes_namespace: Vec<String>,
    exposes_targets: Option<RawExposesTargets>,
    /// String entries; the `{ file, strip }` table form is reserved (caught
    /// here as a Value so the error can say so).
    #[serde(default)]
    patches: Vec<toml::Value>,
    subdir: Option<String>,
    system_includes: Option<bool>,
    /// Reserved (§5.3): pkg-config resolution mode.
    pkg_config: Option<toml::Value>,
    /// Rejected, not reserved (§0.2): inline conditionals.
    when: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawExposesTargets {
    List(Vec<String>),
    Map(BTreeMap<String, String>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTarget {
    #[serde(rename = "type")]
    kind: String,
    sources: Vec<String>,
    cxx_std: Option<u32>,
    c_std: Option<u32>,
    includes: Option<RawVisibility>,
    defines: Option<RawVisibility>,
    dependencies: Option<RawVisibility>,
    cxx_flags: Option<RawVisibility>,
    c_flags: Option<RawVisibility>,
    link_flags: Option<RawVisibility>,
    system_includes: Option<bool>,
    dev: Option<bool>,
    test: Option<bool>,
    install: Option<bool>,
    public_headers: Option<RawPublicHeaders>,
    runtime_data: Option<Vec<RawRuntimeData>>,
    run: Option<Vec<RawRunEntry>>,
    cfg: Option<OrderedTable<toml::Value>>,
    /// Rejected, not reserved (§0.2).
    when: Option<toml::Value>,
    /// Reserved spellings (§9): declared so each names its future instead of
    /// drawing serde's generic unknown-key error.
    exceptions: Option<toml::Value>,
    rtti: Option<toml::Value>,
    frameworks: Option<toml::Value>,
    cxx_extensions: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPublicHeaders {
    base: String,
    patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRuntimeData {
    from: String,
    patterns: Option<Vec<String>>,
    to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRunEntry {
    name: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_remove: Vec<String>,
    #[serde(default)]
    expect_failure: bool,
    /// Reserved (§3.2): CTest-strict signal semantics.
    expect_signal: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawGenerate {
    template: Option<String>,
    output: Option<String>,
    vars: Option<BTreeMap<String, String>>,
    command: Option<Vec<String>>,
    stdin: Option<String>,
    stdout: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    checked_in: Option<String>,
    /// Rejected, not reserved (§0.2).
    when: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawVisibility {
    // Bare list first: an array can never match the table variant, and a
    // table never matches the array, so ordering is not load-bearing — but
    // listing the sugar first keeps the common case cheap.
    Bare(Vec<String>),
    Split(RawSplit),
}

// Separate struct (not an inline variant) because deny_unknown_fields is a
// container attribute — on an inline untagged variant a typo'd key would be
// silently ignored instead of rejected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[derive(Default)]
struct RawSplit {
    public: Vec<String>,
    private: Vec<String>,
}

impl RawVisibility {
    fn into_split(self) -> VisibilitySplit {
        match self {
            RawVisibility::Bare(list) => VisibilitySplit {
                public: Vec::new(),
                private: list,
            },
            RawVisibility::Split(RawSplit { public, private }) => {
                VisibilitySplit { public, private }
            }
        }
    }
}

/// A table deserialized as (key, value) pairs in DOCUMENT ORDER. cfg groups
/// merge in document order (spec §2.2), which a BTreeMap would silently
/// alphabetize — the toml deserializer itself yields entries in document
/// order, so collecting into a Vec preserves it.
#[derive(Debug)]
struct OrderedTable<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OrderedTable<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = OrderedTable<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut m: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut v = Vec::new();
                while let Some((k, val)) = m.next_entry::<String, T>()? {
                    v.push((k, val));
                }
                Ok(OrderedTable(v))
            }
        }
        d.deserialize_map(V(std::marker::PhantomData))
    }
}

// ---------------------------------------------------------------------------
// Interpolation placement (spec §0.3): `${` is legal ONLY in whitelisted
// positions. Resolution happens later (crate::interp); at load we police
// placement over the raw document tree so nothing can slip through a typed
// field we forgot about.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Pat {
    L(&'static str),
    AnyKey,
    AnyIndex,
}

#[derive(Clone)]
struct Seg {
    name: String,
    index: bool,
}

/// Positions where `${...}` may appear at all. `Define` additionally
/// restricts interpolation to the value part (after the first `=`).
#[derive(Clone, Copy, PartialEq)]
enum InterpSlot {
    Define,
    Free,
    Forbidden,
}

fn path_matches(path: &[Seg], pat: &[Pat]) -> bool {
    path.len() == pat.len()
        && path.iter().zip(pat).all(|(s, p)| match p {
            Pat::L(lit) => !s.index && s.name == *lit,
            Pat::AnyKey => !s.index,
            Pat::AnyIndex => s.index,
        })
}

fn classify_interp_slot(path: &[Seg]) -> InterpSlot {
    use Pat::{AnyIndex as I, AnyKey as K, L};
    // The three visibility spellings of a list key under a prefix.
    fn vis_match(path: &[Seg], prefix: &[Pat], key: &'static str) -> bool {
        let mut p = prefix.to_vec();
        p.push(L(key));
        let bare = [p.as_slice(), &[I]].concat();
        let pub_ = [p.as_slice(), &[L("public"), I]].concat();
        let priv_ = [p.as_slice(), &[L("private"), I]].concat();
        path_matches(path, &bare) || path_matches(path, &pub_) || path_matches(path, &priv_)
    }
    let target: &[Pat] = &[L("targets"), K];
    let target_cfg: &[Pat] = &[L("targets"), K, L("cfg"), K];
    let defaults: &[Pat] = &[L("target-defaults")];

    for prefix in [target, target_cfg, defaults] {
        if vis_match(path, prefix, "defines") {
            return InterpSlot::Define;
        }
        if vis_match(path, prefix, "includes") {
            return InterpSlot::Free;
        }
    }
    let free: &[&[Pat]] = &[
        &[L("targets"), K, L("sources"), I],
        &[L("targets"), K, L("cfg"), K, L("sources"), I],
        &[L("generate"), K, L("vars"), K],
        &[L("generate"), K, L("command"), I],
        &[L("targets"), K, L("run"), I, L("args"), I],
        &[L("targets"), K, L("run"), I, L("cwd")],
        &[L("targets"), K, L("run"), I, L("env"), K],
    ];
    if free.iter().any(|p| path_matches(path, p)) {
        return InterpSlot::Free;
    }
    InterpSlot::Forbidden
}

fn display_path(path: &[Seg]) -> String {
    let mut out = String::new();
    for s in path {
        if s.index {
            out.push_str(&format!("[{}]", s.name));
        } else {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(&s.name);
        }
    }
    out
}

fn check_interp_placement(doc: &toml::Value) -> Result<()> {
    let mut path: Vec<Seg> = Vec::new();
    walk_interp(doc, &mut path)
}

fn walk_interp(value: &toml::Value, path: &mut Vec<Seg>) -> Result<()> {
    match value {
        toml::Value::String(s) => {
            if !interp::contains_interp(s) {
                return Ok(());
            }
            match classify_interp_slot(path) {
                InterpSlot::Free => Ok(()),
                InterpSlot::Define => {
                    // Interpolation lives in the VALUE part of a define.
                    match s.split_once('=') {
                        Some((key, _)) if !interp::contains_interp(key) => Ok(()),
                        _ => bail!(
                            "{}: interpolation in a define is only legal in the \
                             value part (after '='): \"{s}\"",
                            display_path(path)
                        ),
                    }
                }
                InterpSlot::Forbidden => {
                    // §6.4: name the sanctioned route for the two positions
                    // people will reach for first.
                    let p = display_path(path);
                    if p.contains("public-headers") || p.contains("runtime-data") {
                        bail!(
                            "{p}: '${{' is not available here in v1 (the §0.3 \
                             position table is exhaustive); generated public \
                             headers ship via ${{gen}} dirs in includes.public"
                        );
                    }
                    bail!(
                        "{p}: '${{...}}' interpolation is not available in this \
                         position; legal positions: defines values, \
                         sources/includes entries, [generate.*] vars/argv, \
                         run-entry args/cwd/env values ('$${{' escapes a \
                         literal '${{')"
                    )
                }
            }
        }
        toml::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                path.push(Seg { name: i.to_string(), index: true });
                walk_interp(item, path)?;
                path.pop();
            }
            Ok(())
        }
        toml::Value::Table(table) => {
            for (k, v) in table {
                path.push(Seg { name: k.clone(), index: false });
                walk_interp(v, path)?;
                path.pop();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// cfg predicate parsing (spec §2.1)
// ---------------------------------------------------------------------------

const CFG_VOCABULARY: &str =
    "os — windows, macos, linux; family — unix; compiler — clang, gcc, msvc";

fn parse_cfg_predicate(context: &str, key: &str) -> Result<CfgPredicate> {
    if let Some(atom) = CfgAtom::from_key(key) {
        return Ok(CfgPredicate { atom });
    }
    if key == "apple-clang" {
        bail!(
            "{context}: cfg atom 'apple-clang' is reserved, not available in \
             v1 — 'clang' matches Apple clang too"
        );
    }
    if key.contains(['(', ')', ',', ' ', '<', '>', '=', '!', '&', '|']) {
        bail!(
            "{context}: cfg combinators and version comparisons ('{key}') are \
             reserved, not available in v1; the future spelling is the quoted \
             key (cfg.\"all(linux, gcc)\") — v1 predicates are single atoms \
             ({CFG_VOCABULARY})"
        );
    }
    bail!("{context}: unknown cfg atom '{key}'; the v1 vocabulary is: {CFG_VOCABULARY}");
}

const WHEN_REJECTED: &str = "inline `when = \"...\"` conditionals are not part of the \
     language (rejected, not reserved — one spelling of a conditional per \
     language); use a `cfg.<predicate>` sub-table";

// ---------------------------------------------------------------------------
// Validation pass: Raw* -> public types.
// ---------------------------------------------------------------------------

fn check_charset(what: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !ok {
        bail!(
            "{what} '{name}' is invalid: names must be non-empty and use only \
             [a-zA-Z0-9_-] (no '::' or '/'; qualifier syntax is reserved)"
        );
    }
    Ok(())
}

fn convert_source(dep_key: &str, raw: &RawDependency) -> Result<SourceSpec> {
    if raw.system == Some(true) {
        if raw.git.is_some()
            || raw.url.is_some()
            || raw.tag.is_some()
            || raw.rev.is_some()
            || raw.sha256.is_some()
        {
            bail!(
                "dependency '{dep_key}': `system = true` is mutually exclusive \
                 with git/url source fields — a system dependency resolves \
                 from the machine and is never fetched"
            );
        }
        return Ok(SourceSpec::System {
            min_version: raw.min_version.clone(),
        });
    }
    match (&raw.git, &raw.url) {
        (Some(_), Some(_)) => bail!(
            "dependency '{dep_key}': both `git` and `url` given; a source is \
             exactly one of git (+ tag or rev) or url (+ sha256)"
        ),
        (None, None) => bail!(
            "dependency '{dep_key}': no source; specify either \
             git = \"<url>\" with tag/rev, url = \"<url>\" with sha256, or \
             system = true"
        ),
        (Some(git), None) => {
            if raw.sha256.is_some() {
                bail!(
                    "dependency '{dep_key}': `sha256` only applies to `url` \
                     sources (git sources are pinned by commit in CppPkg.lock)"
                );
            }
            let reference = match (&raw.tag, &raw.rev) {
                (Some(_), Some(_)) => bail!(
                    "dependency '{dep_key}': both `tag` and `rev` given; a git \
                     source takes exactly one of them"
                ),
                (Some(tag), None) => GitRef::Tag(tag.clone()),
                (None, Some(rev)) => GitRef::Rev(rev.clone()),
                (None, None) => bail!(
                    "dependency '{dep_key}': git source needs `tag = \"...\"` \
                     or `rev = \"<commit sha>\"`"
                ),
            };
            Ok(SourceSpec::Git {
                url: git.clone(),
                reference,
            })
        }
        (None, Some(url)) => {
            if raw.tag.is_some() || raw.rev.is_some() {
                bail!(
                    "dependency '{dep_key}': `tag`/`rev` only apply to `git` \
                     sources"
                );
            }
            let sha256 = raw.sha256.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "dependency '{dep_key}': url source needs \
                     `sha256 = \"<64 hex chars>\"`"
                )
            })?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!(
                    "dependency '{dep_key}': `sha256` must be 64 hex \
                     characters (got {} chars)",
                    sha256.len()
                );
            }
            Ok(SourceSpec::Url {
                url: url.clone(),
                sha256,
            })
        }
    }
}

fn convert_exposes_targets(raw: Option<RawExposesTargets>) -> ExposesTargets {
    match raw {
        None => ExposesTargets::default(),
        Some(RawExposesTargets::List(claims)) => ExposesTargets {
            claims,
            renames: BTreeMap::new(),
        },
        Some(RawExposesTargets::Map(renames)) => ExposesTargets {
            // A rename is also a claim: the map form supersets the list form.
            claims: renames.keys().cloned().collect(),
            renames,
        },
    }
}

/// v1 builtin pseudo-package list (spec §0.6/§5.4): resolution-ladder step 0,
/// unclaimable, referenceable with zero declaration.
pub const BUILTIN_TARGETS: &[&str] = &["Threads::Threads"];
const BUILTIN_NAMESPACES: &[&str] = &["Threads"];

fn convert_dependency(
    dep_key: &str,
    raw: RawDependency,
    cfg: Option<CfgPredicate>,
    dev: bool,
) -> Result<DependencySpec> {
    if raw.when.is_some() {
        bail!("dependency '{dep_key}': {WHEN_REJECTED}");
    }
    if raw.pkg_config.is_some() {
        bail!(
            "dependency '{dep_key}': `pkg-config` is reserved, not implemented \
             in v1 (when it lands it switches a system dependency's resolution \
             from find_package to pkg-config)"
        );
    }

    let source = convert_source(dep_key, &raw)?;
    let is_system = matches!(source, SourceSpec::System { .. });

    if is_system {
        if !raw.patches.is_empty() {
            bail!("dependency '{dep_key}': system dependencies have no source tree to patch");
        }
        if !raw.options.is_empty() {
            bail!(
                "dependency '{dep_key}': `options` are CMake cache options for \
                 a dependency build; a system dependency is never built"
            );
        }
        if !raw.needs.is_empty() {
            bail!(
                "dependency '{dep_key}': `needs` on a system dependency is an \
                 error (other dependencies may `needs` it, and targets may \
                 reference its exported targets)"
            );
        }
        if raw.subdir.is_some() {
            bail!(
                "dependency '{dep_key}': `subdir` only applies to fetched \
                 (git/url) sources"
            );
        }
    } else if raw.min_version.is_some() {
        bail!(
            "dependency '{dep_key}': `min-version` only applies to \
             `system = true` dependencies (fetched sources are pinned exactly \
             by tag/rev/sha256)"
        );
    }

    // Patches: strings only; the `{ file, strip }` table form is reserved.
    let mut patches: Vec<PathBuf> = Vec::new();
    let mut seen_patches: BTreeSet<String> = BTreeSet::new();
    for value in raw.patches {
        let path = match value {
            toml::Value::String(s) => s,
            toml::Value::Table(_) => bail!(
                "dependency '{dep_key}': the `{{ file, strip }}` patch table \
                 form is reserved, not available in v1; use a plain string \
                 path (strip is fixed at 1)"
            ),
            other => bail!(
                "dependency '{dep_key}': `patches` entries must be string \
                 paths (got {})",
                other.type_str()
            ),
        };
        if Path::new(&path).is_absolute() {
            bail!(
                "dependency '{dep_key}': patch path '{path}' must be relative \
                 to the manifest's directory"
            );
        }
        if !seen_patches.insert(path.clone()) {
            bail!("dependency '{dep_key}': duplicate patch '{path}'");
        }
        patches.push(PathBuf::from(path));
    }

    if let Some(subdir) = &raw.subdir {
        let p = Path::new(subdir);
        let bad = subdir.is_empty()
            || p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        if bad {
            bail!(
                "dependency '{dep_key}': `subdir` '{subdir}' must be a \
                 non-empty relative path without '..' (the configure root is \
                 <checkout>/<subdir>)"
            );
        }
    }

    let exposes_targets = convert_exposes_targets(raw.exposes_targets);
    for claim in &exposes_targets.claims {
        if BUILTIN_TARGETS.contains(&claim.as_str()) {
            bail!(
                "dependency '{dep_key}': exposes-targets claims '{claim}', a \
                 builtin pseudo-package; delete this line (builtins resolve \
                 first and cannot be shadowed)"
            );
        }
    }
    for ns in &raw.exposes_namespace {
        if BUILTIN_NAMESPACES.contains(&ns.as_str()) {
            bail!(
                "dependency '{dep_key}': exposes-namespace claims '{ns}', the \
                 namespace of a builtin pseudo-package; delete this line \
                 (builtins resolve first and cannot be shadowed)"
            );
        }
    }

    Ok(DependencySpec {
        source,
        options: raw.options,
        needs: raw.needs,
        find_package: raw.find_package,
        exposes_namespace: raw.exposes_namespace,
        exposes_targets,
        patches,
        subdir: raw.subdir,
        system_includes: raw.system_includes,
        cfg,
        dev,
    })
}

// -- cfg group conversion ---------------------------------------------------

fn value_into<T: serde::de::DeserializeOwned>(
    context: &str,
    key: &str,
    value: toml::Value,
) -> Result<T> {
    value
        .try_into::<T>()
        .with_context(|| format!("{context}: invalid value for '{key}'"))
}

fn convert_flags_cfg_group(pred_key: &str, table: toml::Table) -> Result<PackageFlagsGroup> {
    let ctx = format!("[flags.cfg.{pred_key}]");
    let mut group = PackageFlagsGroup::default();
    for (key, value) in table {
        match key.as_str() {
            "cxx-flags" => group.cxx_flags = value_into(&ctx, &key, value)?,
            "c-flags" => group.c_flags = value_into(&ctx, &key, value)?,
            "link-flags" => group.link_flags = value_into(&ctx, &key, value)?,
            "cfg" => bail!("{ctx}: nested cfg inside a cfg group is an error"),
            "when" => bail!("{ctx}: {WHEN_REJECTED}"),
            other => bail!(
                "{ctx}: key '{other}' is not conditionable here ([flags] has \
                 cxx-flags, c-flags, link-flags)"
            ),
        }
    }
    Ok(group)
}

fn convert_target_cfg_group(
    target: &str,
    pred_key: &str,
    table: toml::Table,
    warnings: &mut Warnings,
) -> Result<TargetCfgGroup> {
    let ctx = format!("target '{target}', [cfg.{pred_key}]");
    let mut group = TargetCfgGroup::default();
    let mut any = false;
    for (key, value) in table {
        any = true;
        match key.as_str() {
            "sources" => group.sources = value_into(&ctx, &key, value)?,
            "includes" => {
                group.includes = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "defines" => {
                group.defines = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "dependencies" => {
                group.dependencies = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "cxx-flags" => {
                group.cxx_flags = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "c-flags" => {
                group.c_flags = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "link-flags" => {
                group.link_flags = value_into::<RawVisibility>(&ctx, &key, value)?.into_split()
            }
            "runtime-data" => {
                let raw: Vec<RawRuntimeData> = value_into(&ctx, &key, value)?;
                group.runtime_data = raw
                    .into_iter()
                    .map(|r| convert_runtime_data(&ctx, r))
                    .collect::<Result<_>>()?;
            }
            "public-headers" => bail!(
                "{ctx}: `public-headers` is a total override and cannot merge \
                 additively under cfg; condition `includes.public` instead — \
                 header derivation follows the cfg projection"
            ),
            "cfg" => bail!("{ctx}: nested cfg inside a cfg group is an error"),
            "when" => bail!("{ctx}: {WHEN_REJECTED}"),
            "dev" | "test" => bail!(
                "{ctx}: `{key}` markers are not cfg-conditional (graph \
                 membership cannot vary by platform)"
            ),
            "run" => bail!("{ctx}: run entries are not cfg-conditional"),
            "cxx-std" | "c-std" | "install" | "system-includes" | "type" => bail!(
                "{ctx}: `{key}` is a scalar; conditional scalar overrides are \
                 not in v1"
            ),
            other => bail!(
                "{ctx}: key '{other}' is not conditionable (list-valued keys \
                 only: sources, includes, defines, dependencies, cxx-flags, \
                 c-flags, link-flags, runtime-data)"
            ),
        }
    }
    if !any {
        warnings
            .0
            .push(format!("[targets.{target}.cfg.{pred_key}] is empty (no effect)"));
    }
    Ok(group)
}

// -- runtime-data / public-headers / run entries ---------------------------

fn convert_runtime_data(context: &str, raw: RawRuntimeData) -> Result<RuntimeData> {
    let from = raw.from.trim_end_matches('/').to_string();
    if from.is_empty() {
        bail!("{context}: runtime-data `from` must be a non-empty directory path");
    }
    let patterns = raw.patterns.unwrap_or_else(|| vec!["**/*".to_string()]);
    check_patterns(&format!("{context}: runtime-data patterns"), &patterns)?;
    let to = match raw.to {
        Some(t) if !t.is_empty() => t,
        Some(_) => bail!("{context}: runtime-data `to` must be non-empty"),
        None => from
            .rsplit('/')
            .next()
            .expect("rsplit yields at least one piece")
            .to_string(),
    };
    Ok(RuntimeData { from, patterns, to })
}

fn convert_public_headers(context: &str, raw: RawPublicHeaders) -> Result<PublicHeaders> {
    if raw.patterns.is_empty() {
        bail!("{context}: public-headers `patterns` must be non-empty");
    }
    check_patterns(&format!("{context}: public-headers patterns"), &raw.patterns)?;
    Ok(PublicHeaders {
        base: raw.base,
        patterns: raw.patterns,
    })
}

/// §0.4: a non-empty pattern list that is ONLY `!`-negations can never match
/// anything — schema error (expansion itself is graph's job).
fn check_patterns(context: &str, patterns: &[String]) -> Result<()> {
    if !patterns.is_empty() && patterns.iter().all(|p| p.starts_with('!')) {
        bail!(
            "{context}: a pattern list of only '!' negations matches nothing; \
             at least one positive pattern is required"
        );
    }
    Ok(())
}

fn convert_run_entries(target: &str, raw: Vec<RawRunEntry>) -> Result<Vec<RunEntry>> {
    let mut entries = Vec::with_capacity(raw.len());
    let mut names: BTreeSet<String> = BTreeSet::new();
    for r in raw {
        if r.expect_signal.is_some() {
            bail!(
                "target '{target}': run entry: `expect-signal` is reserved, \
                 not available in v1 (`expect-failure = true` already passes \
                 on signal death)"
            );
        }
        if let Some(name) = &r.name
            && !names.insert(name.clone())
        {
            bail!("target '{target}': duplicate run entry name '{name}'");
        }
        entries.push(RunEntry {
            name: r.name,
            args: r.args,
            cwd: r.cwd,
            env: r.env,
            env_remove: r.env_remove,
            expect_failure: r.expect_failure,
        });
    }
    Ok(entries)
}

// -- generate steps ---------------------------------------------------------

fn check_gen_out_path(step: &str, field: &str, path: &str) -> Result<()> {
    let p = Path::new(path);
    let bad = path.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if bad {
        bail!(
            "[generate.{step}]: {field} '{path}' must be a relative path \
             without '..' — outputs land under build/gen and source-tree \
             writes are refused by construction"
        );
    }
    Ok(())
}

fn convert_generate(name: &str, raw: RawGenerate) -> Result<GenerateStep> {
    check_charset("generate step name", name)?;
    if raw.when.is_some() {
        bail!("[generate.{name}]: {WHEN_REJECTED}");
    }
    let action = match (&raw.template, &raw.command) {
        (Some(_), Some(_)) => bail!(
            "[generate.{name}]: both `template` and `command` given; a step is \
             exactly one of them"
        ),
        (None, None) => bail!(
            "[generate.{name}]: no action; a step is exactly one of \
             `template = \"...\"` (+ output) or `command = [...]` (+ stdout)"
        ),
        (Some(template), None) => {
            if raw.stdin.is_some() || raw.stdout.is_some() {
                bail!(
                    "[generate.{name}]: `stdin`/`stdout` only apply to \
                     `command` steps; template steps use `output`"
                );
            }
            let output = raw.output.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "[generate.{name}]: template steps need \
                     `output = \"<path under ${{gen}}>\"`"
                )
            })?;
            check_gen_out_path(name, "output", &output)?;
            GenerateAction::Template {
                template: template.clone(),
                output,
                vars: raw.vars.clone().unwrap_or_default(),
            }
        }
        (None, Some(argv)) => {
            if raw.output.is_some() || raw.vars.is_some() {
                bail!(
                    "[generate.{name}]: `output`/`vars` only apply to \
                     `template` steps; command steps use `stdout`"
                );
            }
            if argv.is_empty() {
                bail!("[generate.{name}]: `command` must be a non-empty argv array");
            }
            let stdout = raw.stdout.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "[generate.{name}]: command steps need \
                     `stdout = \"<path under ${{gen}}>\"` (declared outputs \
                     are how consumers reference the step)"
                )
            })?;
            check_gen_out_path(name, "stdout", &stdout)?;
            GenerateAction::Command {
                argv: argv.clone(),
                stdin: raw.stdin.clone(),
                stdout,
            }
        }
    };
    if let Some(checked_in) = &raw.checked_in {
        let p = Path::new(checked_in);
        let bad = checked_in.is_empty()
            || p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
        if bad {
            bail!(
                "[generate.{name}]: checked-in path '{checked_in}' must be a \
                 relative path inside the project (it is the one sanctioned \
                 source-tree write, via `cpp-pkg gen`)"
            );
        }
    }
    Ok(GenerateStep {
        name: name.to_string(),
        action,
        inputs: raw.inputs,
        checked_in: raw.checked_in,
    })
}

// -- target defaults --------------------------------------------------------

#[derive(Debug, Default)]
struct TargetDefaults {
    cxx_std: Option<u32>,
    c_std: Option<u32>,
    defines: VisibilitySplit,
    includes: VisibilitySplit,
    system_includes: Option<bool>,
    install: Option<bool>,
    public_headers: Option<PublicHeaders>,
    runtime_data: Vec<RuntimeData>,
}

fn convert_target_defaults(table: &toml::Table) -> Result<TargetDefaults> {
    const CTX: &str = "[target-defaults]";
    let mut d = TargetDefaults::default();
    for (key, value) in table {
        let value = value.clone();
        match key.as_str() {
            "cxx-std" => d.cxx_std = Some(value_into(CTX, key, value)?),
            "c-std" => d.c_std = Some(value_into(CTX, key, value)?),
            "defines" => {
                d.defines = value_into::<RawVisibility>(CTX, key, value)?.into_split()
            }
            "includes" => {
                d.includes = value_into::<RawVisibility>(CTX, key, value)?.into_split()
            }
            "system-includes" => d.system_includes = Some(value_into(CTX, key, value)?),
            "install" => d.install = Some(value_into(CTX, key, value)?),
            "public-headers" => {
                let raw: RawPublicHeaders = value_into(CTX, key, value)?;
                d.public_headers = Some(convert_public_headers(CTX, raw)?);
            }
            "runtime-data" => {
                let raw: Vec<RawRuntimeData> = value_into(CTX, key, value)?;
                d.runtime_data = raw
                    .into_iter()
                    .map(|r| convert_runtime_data(CTX, r))
                    .collect::<Result<_>>()?;
            }
            "cxx-flags" | "c-flags" | "link-flags" => bail!(
                "{CTX}: `{key}` is reserved here, not available in v1 — \
                 [flags] is the single home for flags every target gets"
            ),
            "cfg" => bail!("[target-defaults.cfg.*] is reserved, not available in v1"),
            "dependencies" => bail!(
                "{CTX}: `dependencies` cannot be defaulted (inherited edges \
                 make dependency graphs unreadable)"
            ),
            "dev" | "test" | "run" => bail!(
                "{CTX}: `{key}` cannot be defaulted (a default that \
                 reclassifies graph membership lies)"
            ),
            "sources" | "type" => bail!("{CTX}: `{key}` cannot be defaulted"),
            other => bail!(
                "{CTX}: unknown key '{other}' (accepted: cxx-std, c-std, \
                 defines, includes, system-includes, install, public-headers, \
                 runtime-data)"
            ),
        }
    }
    Ok(d)
}

/// Prepend `defaults` entries before `own` entries (spec §0.5: "every target
/// gets these" stays true when a target adds its own).
fn prepend_split(defaults: &VisibilitySplit, own: &mut VisibilitySplit) {
    if !defaults.public.is_empty() {
        let mut merged = defaults.public.clone();
        merged.append(&mut own.public);
        own.public = merged;
    }
    if !defaults.private.is_empty() {
        let mut merged = defaults.private.clone();
        merged.append(&mut own.private);
        own.private = merged;
    }
}

// -- targets ---------------------------------------------------------------

fn convert_target(
    name: &str,
    raw: RawTarget,
    defaults: &TargetDefaults,
    warnings: &mut Warnings,
) -> Result<TargetSpec> {
    if raw.when.is_some() {
        bail!("target '{name}': {WHEN_REJECTED}");
    }
    if raw.exceptions.is_some() || raw.rtti.is_some() {
        bail!(
            "target '{name}': `exceptions`/`rtti` knob sugar is reserved, not \
             available in v1; spell the flags directly (e.g. \
             cxx-flags = {{ public = [\"-fno-exceptions\"] }})"
        );
    }
    if raw.frameworks.is_some() {
        bail!(
            "target '{name}': `frameworks` is reserved, not available in v1; \
             use link-flags (e.g. [\"-Wl,-framework,CoreFoundation\"] under \
             a cfg.macos group)"
        );
    }
    if raw.cxx_extensions.is_some() {
        bail!(
            "target '{name}': `cxx-extensions` is reserved, not available in \
             v1 (cxx-std is strict -std=c++NN; gnu++NN dialects come later)"
        );
    }
    let kind = match raw.kind.as_str() {
        "executable" => TargetKind::Executable,
        "static-library" => TargetKind::StaticLibrary,
        other => bail!(
            "target '{name}': unknown type '{other}' \
             (v0 supports: executable, static-library)"
        ),
    };

    // Markers (§3.2): `test` implies `dev`; the explicit contradiction is a
    // reserved spelling, not a silent fixup.
    let test = raw.test.unwrap_or(false);
    if test && raw.dev == Some(false) {
        bail!(
            "target '{name}': `test = true, dev = false` is reserved \
             (dual-role shipped-and-smoke-tested binaries are not in v1); \
             test implies dev"
        );
    }
    if test && kind != TargetKind::Executable {
        bail!(
            "target '{name}': `test = true` is only legal on executables; \
             libraries use `dev = true`"
        );
    }
    let dev = raw.dev.unwrap_or(false) || test;

    let run = convert_run_entries(name, raw.run.unwrap_or_default())?;
    if !run.is_empty() && !test {
        bail!(
            "target '{name}': [[run]] entries are only legal on test targets \
             (set `test = true`)"
        );
    }

    let public_headers = raw
        .public_headers
        .map(|ph| convert_public_headers(&format!("target '{name}'"), ph))
        .transpose()?;

    let runtime_data = raw
        .runtime_data
        .unwrap_or_default()
        .into_iter()
        .map(|r| convert_runtime_data(&format!("target '{name}'"), r))
        .collect::<Result<Vec<_>>>()?;

    let mut cfg_groups: Vec<(CfgPredicate, TargetCfgGroup)> = Vec::new();
    if let Some(OrderedTable(entries)) = raw.cfg {
        for (pred_key, value) in entries {
            let pred = parse_cfg_predicate(&format!("target '{name}'"), &pred_key)?;
            let table = match value {
                toml::Value::Table(t) => t,
                other => bail!(
                    "target '{name}': [cfg.{pred_key}] must be a table of \
                     conditionable keys (got {})",
                    other.type_str()
                ),
            };
            let group = convert_target_cfg_group(name, &pred_key, table, warnings)?;
            cfg_groups.push((pred, group));
        }
    }

    let mut t = TargetSpec {
        kind,
        sources: raw.sources,
        cxx_std: raw.cxx_std,
        c_std: raw.c_std,
        includes: raw.includes.map(RawVisibility::into_split).unwrap_or_default(),
        defines: raw.defines.map(RawVisibility::into_split).unwrap_or_default(),
        dependencies: raw
            .dependencies
            .map(RawVisibility::into_split)
            .unwrap_or_default(),
        cxx_flags: raw
            .cxx_flags
            .map(RawVisibility::into_split)
            .unwrap_or_default(),
        c_flags: raw.c_flags.map(RawVisibility::into_split).unwrap_or_default(),
        link_flags: raw
            .link_flags
            .map(RawVisibility::into_split)
            .unwrap_or_default(),
        system_includes: raw.system_includes,
        dev,
        test,
        install: raw.install.unwrap_or(false),
        public_headers,
        runtime_data,
        run,
        cfg: cfg_groups,
    };

    // ---- [target-defaults] merge (spec §7.2, merge rules §0.5) ----------
    // Scalars fill-if-absent (an explicit target value, true OR false, wins);
    // list/visibility keys prepend; eligibility comes from the target's own
    // markers and kind, so a package-wide `install = true` skips dev/test
    // targets instead of manufacturing §6.4's error at scale.
    t.cxx_std = t.cxx_std.or(defaults.cxx_std);
    t.c_std = t.c_std.or(defaults.c_std);
    t.system_includes = t.system_includes.or(defaults.system_includes);
    prepend_split(&defaults.defines, &mut t.defines);
    prepend_split(&defaults.includes, &mut t.includes);
    if !defaults.runtime_data.is_empty() {
        let mut merged = defaults.runtime_data.clone();
        merged.append(&mut t.runtime_data);
        t.runtime_data = merged;
    }
    let install_eligible = !t.dev && !t.test;
    if raw.install.is_none() && install_eligible {
        t.install = defaults.install.unwrap_or(false);
    }
    if t.public_headers.is_none()
        && install_eligible
        && t.kind == TargetKind::StaticLibrary
        && t.install
    {
        t.public_headers = defaults.public_headers.clone();
    }

    Ok(t)
}

// -- the propagation fence (spec §1.2/§1.4) ---------------------------------

/// Dedicated-key advice for spellings that have a schema home. A warning,
/// not an error: migrations paste flag soup, and `-UNDEBUG` has no schema
/// home at all (benchmark's tests legitimately need it).
fn dedicated_key_warnings(scope: &str, list_name: &str, words: &[String], warnings: &mut Warnings) {
    for w in words {
        let hint = if w.starts_with("-D") || w.starts_with("-U") {
            Some("defines")
        } else if w.starts_with("-I") || w.starts_with("-isystem") {
            Some("includes")
        } else if w.starts_with("-std=") {
            Some("cxx-std / c-std")
        } else {
            None
        };
        if let Some(hint) = hint {
            warnings.0.push(format!(
                "{scope}: {list_name} contains '{w}'; prefer the dedicated \
                 key ({hint})"
            ));
        }
    }
}

/// Fence one flag list of one target (spec §1.2). `public` selects the
/// bucket rules; ABI-classified words are rejected in EVERY bucket (§1.4).
/// `link` relaxes the warning/opt-debug checks (only ABI/sanitizer are
/// categorically wrong to propagate on a link line).
fn fence_flag_list(
    target: &str,
    list_name: &str,
    public: bool,
    link: bool,
    words: &[String],
    warnings: &mut Warnings,
) -> Result<()> {
    use crate::toolchain::FlagClass;
    dedicated_key_warnings(&format!("target '{target}'"), list_name, words, warnings);
    for cw in crate::toolchain::classify_word_sequence(words) {
        let word = &words[cw.index];
        let spelled = if &cw.payload == word {
            format!("'{word}'")
        } else {
            format!("'{word}' (carrying '{}')", cw.payload)
        };
        match cw.class {
            FlagClass::Abi => bail!(
                "target '{target}': {list_name} contains {spelled}, which \
                 affects the ABI of the entire link closure including store \
                 dependencies; move it to [flags] or a [profiles.*] block, \
                 where it will propagate to dependency builds and their \
                 config hashes"
            ),
            FlagClass::Sanitizer if public => bail!(
                "target '{target}': public {list_name} contains {spelled}: \
                 sanitizer-class flags cannot be public (dependencies and \
                 consumers are built uninstrumented); keep it private or in a \
                 profile"
            ),
            FlagClass::Sanitizer => warnings.0.push(format!(
                "target '{target}': {list_name} contains '{word}', which \
                 applies to this target only — dependencies are built \
                 uninstrumented (ASan interoperates with uninstrumented code; \
                 whole-world instrumentation is out of scope)"
            )),
            FlagClass::Warning if public && !link => bail!(
                "target '{target}': public {list_name} contains {spelled}: \
                 warnings are private by nature; a library cannot volunteer \
                 its consumers into a diagnostic policy"
            ),
            FlagClass::OptDebug if public && !link => bail!(
                "target '{target}': public {list_name} contains {spelled}: \
                 optimization level is the consumer's (profile's) decision"
            ),
            _ => {}
        }
    }
    Ok(())
}

fn validate_target(name: &str, t: &TargetSpec, warnings: &mut Warnings) -> Result<()> {
    // §0.4: an all-negative unconditional source list can never match.
    // (Cfg groups may be all-negative: they refine a non-empty base list.)
    if !t.sources.is_empty() && t.sources.iter().all(|s| s.starts_with('!')) {
        bail!(
            "target '{name}': `sources` contains only '!' negative patterns; \
             at least one positive pattern is required"
        );
    }

    // §6.4 / §3.2: dev/test targets are excluded from export.
    if t.install && t.test {
        bail!(
            "target '{name}': `install = true` on a test target is an error \
             (test targets are excluded from export); remove one of the two"
        );
    }
    if t.install && t.dev {
        bail!(
            "target '{name}': `install = true` on a dev target is an error \
             (dev targets are excluded from export); remove one of the two"
        );
    }

    // §1.2: nothing can consume an executable, so public flags there are a
    // category error (public includes/defines stay legal as in v0 — only the
    // flag surface is fenced).
    if t.kind == TargetKind::Executable {
        let mut flag_lists: Vec<(&str, &VisibilitySplit)> = vec![
            ("cxx-flags", &t.cxx_flags),
            ("c-flags", &t.c_flags),
            ("link-flags", &t.link_flags),
        ];
        for (_, g) in &t.cfg {
            flag_lists.push(("cxx-flags", &g.cxx_flags));
            flag_lists.push(("c-flags", &g.c_flags));
            flag_lists.push(("link-flags", &g.link_flags));
        }
        for (list_name, split) in flag_lists {
            if !split.public.is_empty() {
                bail!(
                    "target '{name}': public {list_name} on an executable is \
                     an error — nothing can consume an executable; make them \
                     private"
                );
            }
        }
    }

    // The propagation fence, over the unconditional lists AND every cfg
    // group (§2.2: non-matching groups are validated, never expanded).
    let groups: Vec<(&VisibilitySplit, &VisibilitySplit, &VisibilitySplit)> =
        std::iter::once((&t.cxx_flags, &t.c_flags, &t.link_flags))
            .chain(t.cfg.iter().map(|(_, g)| (&g.cxx_flags, &g.c_flags, &g.link_flags)))
            .collect();
    for (cxx, c, link) in groups {
        for (list_name, split, is_link) in
            [("cxx-flags", cxx, false), ("c-flags", c, false), ("link-flags", link, true)]
        {
            fence_flag_list(name, list_name, true, is_link, &split.public, warnings)?;
            fence_flag_list(name, list_name, false, is_link, &split.private, warnings)?;
        }
    }
    Ok(())
}

// -- sanitizer warnings (profiles + [flags], same consumer-only semantics) --

fn sanitizer_warnings(scope: &str, lists: &[(&str, &[String])], warnings: &mut Warnings) {
    for (list_name, flags) in lists {
        for flag in *flags {
            if flag.starts_with("-fsanitize") {
                warnings.0.push(format!(
                    "{scope}: {list_name} contains '{flag}', which \
                     applies to consumer targets only — dependencies are built \
                     uninstrumented (ASan interoperates with uninstrumented \
                     code; whole-world instrumentation is out of scope in v0)"
                ));
            }
        }
    }
}

/// Parse + validate a CppPkg.toml. Fails with `schema-version` mismatch,
/// syntax errors, or any validation rule above.
pub fn load(path: &Path) -> Result<(ProjectFile, Warnings)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    parse_str(&text).with_context(|| format!("in {}", path.display()))
}

/// Parse + validate manifest text (the file-free core of [`load`]).
pub fn parse_str(text: &str) -> Result<(ProjectFile, Warnings)> {
    let raw: RawProject = toml::from_str(text).context("CppPkg.toml parse error")?;

    if raw.schema_version != crate::SCHEMA_VERSION {
        bail!(
            "schema-version {} is not supported (this cpp-pkg understands \
             schema-version {})",
            raw.schema_version,
            crate::SCHEMA_VERSION
        );
    }

    // Interpolation placement (§0.3) is policed over the raw value tree so
    // no position can slip through untyped.
    let doc: toml::Value = toml::from_str(text).context("CppPkg.toml parse error")?;
    check_interp_placement(&doc)?;

    check_charset("package name", &raw.package.name)?;
    let package = PackageMeta {
        name: raw.package.name,
        version: raw.package.version,
    };

    let export = {
        let raw_export = raw.export.unwrap_or(RawExport { cmake_name: None, namespace: None });
        let cmake_name = raw_export.cmake_name.unwrap_or_else(|| package.name.clone());
        let namespace = raw_export.namespace.unwrap_or_else(|| package.name.clone());
        check_charset("[export] cmake-name", &cmake_name)?;
        check_charset("[export] namespace", &namespace)?;
        ExportMeta { cmake_name, namespace }
    };

    let toolchains = raw
        .toolchains
        .into_iter()
        .map(|(name, t)| {
            (
                name,
                ToolchainPreset {
                    cxx: t.cxx,
                    cc: t.cc,
                    ar: t.ar,
                },
            )
        })
        .collect();

    let mut warnings = Warnings::default();
    let mut profiles = BTreeMap::new();
    for (name, p) in raw.profiles {
        // v0 restriction: only the four built-ins exist; `base-config` custom
        // profiles are reserved so this stays an error, not a silent accept.
        BuildConfig::from_key(&name).map_err(|_| {
            anyhow::anyhow!(
                "profile '{name}' is not a built-in profile; v0 supports only \
                 debug, release, relwithdebinfo, minsizerel (custom profiles \
                 via `base-config` are reserved for a future schema version)"
            )
        })?;
        if p.cfg.is_some() {
            bail!("[profiles.{name}.cfg.*] is reserved, not available in v1");
        }
        let profile = Profile {
            cxx_flags: p.cxx_flags,
            c_flags: p.c_flags,
            link_flags: p.link_flags,
        };
        sanitizer_warnings(
            &format!("profile '{name}'"),
            &[
                ("cxx-flags", &profile.cxx_flags),
                ("c-flags", &profile.c_flags),
                ("link-flags", &profile.link_flags),
            ],
            &mut warnings,
        );
        profiles.insert(name, profile);
    }

    // [flags] (§1.1). No fence here: there is no public bucket at this scope
    // (environment, not interface); ABI entries are the *point* (they inject
    // into dep builds via the existing profile-ABI machinery).
    let flags = match raw.flags {
        None => PackageFlags::default(),
        Some(rf) => {
            let mut cfg_groups = Vec::new();
            if let Some(OrderedTable(entries)) = rf.cfg {
                for (pred_key, value) in entries {
                    let pred = parse_cfg_predicate("[flags]", &pred_key)?;
                    let table = match value {
                        toml::Value::Table(t) => t,
                        other => bail!(
                            "[flags.cfg.{pred_key}] must be a table (got {})",
                            other.type_str()
                        ),
                    };
                    let group = convert_flags_cfg_group(&pred_key, table)?;
                    if group.cxx_flags.is_empty()
                        && group.c_flags.is_empty()
                        && group.link_flags.is_empty()
                    {
                        warnings
                            .0
                            .push(format!("[flags.cfg.{pred_key}] is empty (no effect)"));
                    }
                    sanitizer_warnings(
                        &format!("[flags.cfg.{pred_key}]"),
                        &[
                            ("cxx-flags", &group.cxx_flags),
                            ("c-flags", &group.c_flags),
                            ("link-flags", &group.link_flags),
                        ],
                        &mut warnings,
                    );
                    cfg_groups.push((pred, group));
                }
            }
            let pf = PackageFlags {
                cxx_flags: rf.cxx_flags,
                c_flags: rf.c_flags,
                link_flags: rf.link_flags,
                cfg: cfg_groups,
            };
            sanitizer_warnings(
                "[flags]",
                &[
                    ("cxx-flags", &pf.cxx_flags),
                    ("c-flags", &pf.c_flags),
                    ("link-flags", &pf.link_flags),
                ],
                &mut warnings,
            );
            pf
        }
    };

    // Dependencies: one namespace across [dependencies], [dev-dependencies],
    // and every [cfg.<pred>.…] branch. `declared_at` remembers where each key
    // first appeared so collision errors can name both sites.
    let mut dependencies: BTreeMap<String, DependencySpec> = BTreeMap::new();
    let mut dev_dependencies: BTreeMap<String, DependencySpec> = BTreeMap::new();
    let mut declared_at: BTreeMap<String, String> = BTreeMap::new();

    fn declare_dep(
        dependencies: &mut BTreeMap<String, DependencySpec>,
        dev_dependencies: &mut BTreeMap<String, DependencySpec>,
        declared_at: &mut BTreeMap<String, String>,
        key: String,
        spec: DependencySpec,
        site: String,
    ) -> Result<()> {
        check_charset("dependency key", &key)?;
        if let Some(first) = declared_at.get(&key) {
            let first_dev = first.contains("dev-dependencies");
            let this_dev = site.contains("dev-dependencies");
            if first_dev != this_dev {
                bail!(
                    "'{key}' is declared in both {first} and {site}; \
                     [dependencies] and [dev-dependencies] share one \
                     resolution namespace — pick one table"
                );
            }
            bail!(
                "dependency '{key}' is declared more than once ({first} and \
                 {site}); one declaration per key in v1 — a dependency is \
                 bundled everywhere or system everywhere"
            );
        }
        declared_at.insert(key.clone(), site);
        if spec.dev {
            dev_dependencies.insert(key, spec);
        } else {
            dependencies.insert(key, spec);
        }
        Ok(())
    }

    for (key, d) in raw.dependencies {
        let spec = convert_dependency(&key, d, None, false)?;
        declare_dep(
            &mut dependencies,
            &mut dev_dependencies,
            &mut declared_at,
            key,
            spec,
            "[dependencies]".to_string(),
        )?;
    }
    for (key, d) in raw.dev_dependencies {
        let spec = convert_dependency(&key, d, None, true)?;
        declare_dep(
            &mut dependencies,
            &mut dev_dependencies,
            &mut declared_at,
            key,
            spec,
            "[dev-dependencies]".to_string(),
        )?;
    }
    for (pred_key, scope) in raw.cfg {
        let pred = parse_cfg_predicate("[cfg] scope", &pred_key)?;
        if scope.targets.is_some() {
            bail!(
                "[cfg.{pred_key}.targets.*] (whole conditional targets) is \
                 reserved, not available in v1; condition the target's list \
                 keys via [targets.<t>.cfg.{pred_key}] instead"
            );
        }
        if scope.generate.is_some() {
            bail!("[cfg.{pred_key}.generate.*] is reserved, not available in v1");
        }
        if scope.flags.is_some() {
            bail!(
                "[cfg.{pred_key}.flags] is not a position; conditional \
                 package flags live at [flags.cfg.{pred_key}] (a cfg table \
                 nests inside the scope it conditions)"
            );
        }
        if let Some(k) = scope.other.keys().next() {
            bail!(
                "[cfg.{pred_key}]: unknown key '{k}' (this scope hosts \
                 `dependencies` and `dev-dependencies` tables)"
            );
        }
        if scope.dependencies.is_empty() && scope.dev_dependencies.is_empty() {
            warnings
                .0
                .push(format!("[cfg.{pred_key}] is empty (no effect)"));
        }
        for (key, d) in scope.dependencies {
            let spec = convert_dependency(&key, d, Some(pred), false)?;
            declare_dep(
                &mut dependencies,
                &mut dev_dependencies,
                &mut declared_at,
                key,
                spec,
                format!("[cfg.{pred_key}.dependencies]"),
            )?;
        }
        for (key, d) in scope.dev_dependencies {
            let spec = convert_dependency(&key, d, Some(pred), true)?;
            declare_dep(
                &mut dependencies,
                &mut dev_dependencies,
                &mut declared_at,
                key,
                spec,
                format!("[cfg.{pred_key}.dev-dependencies]"),
            )?;
        }
    }

    // Referential integrity of `needs`, with a message that says how to fix.
    // Regular deps may not reach into the dev graph (§3.2).
    for (key, dep) in &dependencies {
        for need in &dep.needs {
            if dev_dependencies.contains_key(need) {
                bail!(
                    "dependency '{key}': needs '{need}', which is a \
                     [dev-dependencies] key — a regular dependency cannot \
                     need a dev-dependency; move '{need}' to [dependencies]"
                );
            }
            if !dependencies.contains_key(need) {
                bail!(
                    "dependency '{key}': needs '{need}', which is not a key of \
                     [dependencies] — add a [dependencies.{need}] entry or fix \
                     the spelling"
                );
            }
        }
    }
    for (key, dep) in &dev_dependencies {
        for need in &dep.needs {
            if !dependencies.contains_key(need) && !dev_dependencies.contains_key(need) {
                bail!(
                    "dev-dependency '{key}': needs '{need}', which is not a \
                     key of [dependencies] or [dev-dependencies] — add it or \
                     fix the spelling"
                );
            }
        }
    }
    // Cycle check over the combined namespace (the order itself is
    // recomputed by callers when needed).
    {
        let mut combined = dependencies.clone();
        combined.extend(dev_dependencies.iter().map(|(k, v)| (k.clone(), v.clone())));
        dependency_build_order(&combined)?;
    }

    // [generate.*] steps + the case-insensitive output collision check
    // (§4.2: a macOS-authored manifest cannot mean two files on Linux).
    let mut generate: BTreeMap<String, GenerateStep> = BTreeMap::new();
    let mut outputs_ci: BTreeMap<String, String> = BTreeMap::new();
    for (name, g) in raw.generate {
        let step = convert_generate(&name, g)?;
        let normalized = step.action.output().to_lowercase();
        if let Some(first) = outputs_ci.get(&normalized) {
            bail!(
                "[generate] output collision: steps '{first}' and '{name}' \
                 both produce '{}' (outputs are compared case-insensitively \
                 on all platforms)",
                step.action.output()
            );
        }
        outputs_ci.insert(normalized, name.clone());
        generate.insert(name, step);
    }

    let defaults = raw
        .target_defaults
        .as_ref()
        .map(convert_target_defaults)
        .transpose()?
        .unwrap_or_default();

    let mut targets = BTreeMap::new();
    for (name, t) in raw.targets {
        check_charset("target name", &name)?;
        let spec = convert_target(&name, t, &defaults, &mut warnings)?;
        validate_target(&name, &spec, &mut warnings)?;
        targets.insert(name, spec);
    }

    Ok((
        ProjectFile {
            package,
            toolchains,
            profiles,
            flags,
            dependencies,
            dev_dependencies,
            generate,
            export,
            targets,
            target_defaults_raw: raw.target_defaults,
        },
        warnings,
    ))
}

/// Topological order of dependency keys following `needs` edges
/// (dependencies before dependents). Cycle => error naming the cycle.
///
/// Deterministic: among ready nodes the lexicographically smallest key is
/// emitted first, so the order is a pure function of the dependency map.
pub fn dependency_build_order(deps: &BTreeMap<String, DependencySpec>) -> Result<Vec<String>> {
    // Validate edges here too so the function is safe when called standalone
    // (not only through load()'s already-validated data).
    for (key, dep) in deps {
        for need in &dep.needs {
            if !deps.contains_key(need) {
                bail!(
                    "dependency '{key}': needs '{need}', which is not a key of \
                     [dependencies]"
                );
            }
        }
    }

    let mut order = Vec::with_capacity(deps.len());
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    while placed.len() < deps.len() {
        // O(n^2) selection is fine at manifest scale and keeps the
        // smallest-ready-key-first guarantee trivially correct.
        let next = deps.keys().find(|k| {
            !placed.contains(k.as_str())
                && deps[*k].needs.iter().all(|n| placed.contains(n.as_str()))
        });
        match next {
            Some(key) => {
                placed.insert(key);
                order.push(key.clone());
            }
            None => return Err(cycle_error(deps, &placed)),
        }
    }
    Ok(order)
}

/// Every unplaced node has an unplaced `needs` edge (that is what stalled the
/// topo sort), so walking those edges from any unplaced node must revisit a
/// node — that revisit is a concrete cycle we can show the user.
fn cycle_error(deps: &BTreeMap<String, DependencySpec>, placed: &BTreeSet<&str>) -> anyhow::Error {
    let start = deps
        .keys()
        .find(|k| !placed.contains(k.as_str()))
        .expect("cycle_error called with no unplaced nodes");
    let mut path: Vec<&str> = vec![start];
    let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
    index_of.insert(start, 0);
    loop {
        let current = *path.last().unwrap();
        let next = deps[current]
            .needs
            .iter()
            .find(|n| !placed.contains(n.as_str()))
            .expect("unplaced node with all needs placed cannot stall the sort");
        if let Some(&i) = index_of.get(next.as_str()) {
            let mut cycle: Vec<&str> = path[i..].to_vec();
            cycle.push(next);
            return anyhow::anyhow!(
                "'needs' cycle in [dependencies]: {} — remove one of these \
                 needs edges to break the cycle",
                cycle.join(" -> ")
            );
        }
        index_of.insert(next, path.len());
        path.push(next);
    }
}

/// Transitive closure of `needs` for one dependency (for CMAKE_PREFIX_PATH:
/// a loaded fmtConfig.cmake re-runs its own find_dependency calls).
///
/// Deterministic: the closure is returned sorted (lexicographic). Order
/// carries no semantics for CMAKE_PREFIX_PATH entries — each prefix hosts a
/// distinct package config — so sorted is the simplest stable choice.
/// The key itself is not part of its own closure; a self-reachable key is a
/// cycle and errors.
pub fn needs_closure(
    deps: &BTreeMap<String, DependencySpec>,
    key: &str,
) -> Result<Vec<String>> {
    if !deps.contains_key(key) {
        bail!("'{key}' is not a key of [dependencies]");
    }

    // Iterative DFS with an in-stack set so a cycle in the reachable
    // subgraph is reported rather than silently absorbed by a visited-set.
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        InProgress,
        Done,
    }
    let mut state: BTreeMap<&str, State> = BTreeMap::new();
    // Stack frames: (node, next-needs-index to visit).
    let mut stack: Vec<(&str, usize)> = vec![(key, 0)];
    state.insert(key, State::InProgress);

    while let Some(&mut (node, ref mut idx)) = stack.last_mut() {
        let needs = &deps[node].needs;
        if *idx >= needs.len() {
            state.insert(node, State::Done);
            stack.pop();
            continue;
        }
        let child = needs[*idx].as_str();
        *idx += 1;
        if !deps.contains_key(child) {
            bail!(
                "dependency '{node}': needs '{child}', which is not a key of \
                 [dependencies]"
            );
        }
        match state.get(child) {
            Some(State::InProgress) => {
                // Reconstruct the cycle from the DFS stack for the message.
                let pos = stack
                    .iter()
                    .position(|(n, _)| *n == child)
                    .expect("in-progress node must be on the stack");
                let mut cycle: Vec<&str> = stack[pos..].iter().map(|(n, _)| *n).collect();
                cycle.push(child);
                bail!(
                    "'needs' cycle in [dependencies]: {} — remove one of these \
                     needs edges to break the cycle",
                    cycle.join(" -> ")
                );
            }
            Some(State::Done) => {}
            None => {
                state.insert(child, State::InProgress);
                stack.push((child, 0));
            }
        }
    }

    Ok(state
        .into_keys()
        .filter(|k| *k != key)
        .map(str::to_owned)
        .collect())
}
