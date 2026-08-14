//! `CppPkg.toml` types + parsing + validation. Normative spec: CPPKG_TOML.md.
//!
//! Implementation notes (contract):
//! - kebab-case TOML keys (`schema-version`, `cxx-std`, `exposes-namespace`).
//! - `VisibilitySplit` deserializes from EITHER a bare array (=> all private;
//!   sugar applies uniformly to includes/defines/dependencies) OR a table
//!   `{ public = [...], private = [...] }`.
//! - `DependencySpec` source: exactly one of git(+tag|rev) or url(+sha256);
//!   anything else is a validation error.
//! - Validation (all hard errors, with actionable messages):
//!   * charset `[a-zA-Z0-9_-]+` for package name, dependency keys, target names
//!   * `needs` entries must be dependency keys; `needs` cycles are errors
//!   * profile names must be one of the four built-ins (v0)
//!   * ABI-affecting profile flags are ALLOWED (they propagate to deps,
//!     see toolchain::classify_flags); `-fsanitize=*` triggers a warning
//!     (returned in `Warnings`, printed by the CLI)
//!   * unknown TOML keys should be rejected (serde deny_unknown_fields)
//!
//! Parsing strategy: serde deserializes into private `Raw*` structs (which
//! own all the TOML-shape concerns: kebab-case, deny_unknown_fields, the
//! bare-array-or-table sugar), and an explicit validation pass converts them
//! into the serde-free public types. This keeps every validation rule in one
//! auditable place and lets error messages name the offending key.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::Result;

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

#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub package: PackageMeta,
    pub toolchains: BTreeMap<String, ToolchainPreset>,
    pub profiles: BTreeMap<String, Profile>,
    pub dependencies: BTreeMap<String, DependencySpec>,
    pub targets: BTreeMap<String, TargetSpec>,
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

#[derive(Debug, Clone)]
pub enum GitRef {
    Tag(String),
    Rev(String),
}

#[derive(Debug, Clone)]
pub enum SourceSpec {
    Git { url: String, reference: GitRef },
    Url { url: String, sha256: String },
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
    /// (Schema addition over CPPKG_TOML.md — recorded in DESIGN_CHOICES.md.)
    pub find_package: Option<String>,
    pub exposes_namespace: Vec<String>,
    pub exposes_targets: ExposesTargets,
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

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub kind: TargetKind,
    /// Glob patterns; expansion (sorted byte order) happens in graph::plan.
    pub sources: Vec<String>,
    pub cxx_std: Option<u32>,
    pub c_std: Option<u32>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
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
    toolchains: BTreeMap<String, RawToolchain>,
    #[serde(default)]
    profiles: BTreeMap<String, RawProfile>,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
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
}

// Source fields are flat Options rather than a serde enum so that malformed
// combinations (git+url, git without a ref, url without sha256, ...) reach
// the validation pass and get a message naming the dependency — an untagged
// enum would collapse them all into one unhelpful "no variant matched".
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawDependency {
    git: Option<String>,
    tag: Option<String>,
    rev: Option<String>,
    url: Option<String>,
    sha256: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, String>,
    #[serde(default)]
    needs: Vec<String>,
    find_package: Option<String>,
    #[serde(default)]
    exposes_namespace: Vec<String>,
    exposes_targets: Option<RawExposesTargets>,
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
    match (&raw.git, &raw.url) {
        (Some(_), Some(_)) => bail!(
            "dependency '{dep_key}': both `git` and `url` given; a source is \
             exactly one of git (+ tag or rev) or url (+ sha256)"
        ),
        (None, None) => bail!(
            "dependency '{dep_key}': no source; specify either \
             git = \"<url>\" with tag/rev, or url = \"<url>\" with sha256"
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

fn convert_target(name: &str, raw: RawTarget) -> Result<TargetSpec> {
    let kind = match raw.kind.as_str() {
        "executable" => TargetKind::Executable,
        "static-library" => TargetKind::StaticLibrary,
        other => bail!(
            "target '{name}': unknown type '{other}' \
             (v0 supports: executable, static-library)"
        ),
    };
    Ok(TargetSpec {
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
    })
}

fn sanitizer_warnings(name: &str, profile: &Profile, warnings: &mut Warnings) {
    let lists = [
        ("cxx-flags", &profile.cxx_flags),
        ("c-flags", &profile.c_flags),
        ("link-flags", &profile.link_flags),
    ];
    for (list_name, flags) in lists {
        for flag in flags {
            if flag.starts_with("-fsanitize") {
                warnings.0.push(format!(
                    "profile '{name}': {list_name} contains '{flag}', which \
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

    check_charset("package name", &raw.package.name)?;
    let package = PackageMeta {
        name: raw.package.name,
        version: raw.package.version,
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
        let profile = Profile {
            cxx_flags: p.cxx_flags,
            c_flags: p.c_flags,
            link_flags: p.link_flags,
        };
        sanitizer_warnings(&name, &profile, &mut warnings);
        profiles.insert(name, profile);
    }

    let mut dependencies = BTreeMap::new();
    for (key, d) in raw.dependencies {
        check_charset("dependency key", &key)?;
        let source = convert_source(&key, &d)?;
        dependencies.insert(
            key,
            DependencySpec {
                source,
                options: d.options,
                needs: d.needs,
                find_package: d.find_package,
                exposes_namespace: d.exposes_namespace,
                exposes_targets: convert_exposes_targets(d.exposes_targets),
            },
        );
    }

    // Referential integrity of `needs`, with a message that says how to fix.
    for (key, dep) in &dependencies {
        for need in &dep.needs {
            if !dependencies.contains_key(need) {
                bail!(
                    "dependency '{key}': needs '{need}', which is not a key of \
                     [dependencies] — add a [dependencies.{need}] entry or fix \
                     the spelling"
                );
            }
        }
    }
    // Cycle check (the order itself is recomputed by callers when needed).
    dependency_build_order(&dependencies)?;

    let mut targets = BTreeMap::new();
    for (name, t) in raw.targets {
        check_charset("target name", &name)?;
        targets.insert(name.clone(), convert_target(&name, t)?);
    }

    Ok((
        ProjectFile {
            package,
            toolchains,
            profiles,
            dependencies,
            targets,
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

