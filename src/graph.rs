//! Target-graph resolution and build planning.
//!
//! Responsibilities (CPPKG_TOML.md "Semantics" + wave-1 spec):
//! 1. NAMING LADDER for every dependency reference in [targets.*]:
//!    step 0: builtin pseudo-packages (`Threads::Threads`), unshadowable;
//!    then: unique across manifests -> direct; else `<depkey>::` prefix owns;
//!    else exposes-namespace / exposes-targets (mapping form renames); else
//!    HARD ERROR listing candidate owning packages + the exposes-* fix.
//!    Local target names (no "::") resolve to sibling targets.
//! 2. VISIBILITY PROPAGATION: public deps/includes/defines/flags propagate to
//!    consumers; private do not — EXCEPT private deps of a static-library
//!    propagate as LINK-ONLY edges (artifacts reach the final link closure,
//!    compile requirements stop). Manifest `requires` are public edges of
//!    that component; `link_requires` are link-only.
//! 3. SOURCES: expand globs relative to the project root in sorted byte
//!    order; `!`-prefixed negative patterns subtract from the union (§0.4).
//!    Extension table (exhaustive, hard error otherwise):
//!    .cpp .cc .cxx .c++ -> C++ | .c -> C | .C -> error (case-insensitive
//!    FS) | .m .mm -> error ("Objective-C not supported in v0").
//! 4. INTERFACE_SOURCES of consumed components become CompileUnits of the
//!    consuming target (compiled with that component's usage requirements).
//! 5. LINK PLAN: topological order over the closure; static archives
//!    deduped keeping the LAST occurrence; frameworks/system libs deduped
//!    keeping first. Each closure member's archive is immediately followed
//!    by that member's own link-flag words (§1.3: raw `-lrt`-class words
//!    must land after the archive whose objects reference them, or GNU ld
//!    under --as-needed discards the library). Cycles -> error.
//! 6. LINK LANGUAGE: any C++ unit in the target or C++ anywhere in its
//!    closure -> C++ driver links.
//! 7. cxx-std: per-target `cxx-std` max-merged with the max `cxx_std`
//!    required by consumed components.
//! 8. cfg PROJECTION (§2.2): every target is reduced to ONE effective view
//!    (unconditional lists + matching cfg groups appended in document order,
//!    per key and per visibility bucket) and every downstream read — units,
//!    layering, propagation, export metadata — sees only that view.
//! 9. FLAG LAYERING (§1.3, last-wins): [flags] non-ABI (+ matching cfg) ->
//!    profile -> propagated public flags of the compile-visible closure
//!    (dedup by contributing target, never by flag string) -> own public ->
//!    own private. ABI-classified [flags] entries are EXCLUDED here — the
//!    cli injects them (layer 2) and folds them into dep config hashes.
//! 10. DEV GRAPH (§3.2): dev/test targets are excluded from the default
//!     build; a non-dev target may not depend on a dev target or a
//!     dev-dep-owned component.
//! 11. GENERATE (§4.2): steps activate lazily from `${gen}` references in
//!     the requested set; inter-step `${gen}` inputs order them; activated
//!     steps' inputs are existence-checked (dormant ones never).
//! 12. RUNTIME DATA (§6.5): build-time staging plan with byte-equal dedupe.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};

use crate::interp::{self, InterpCtx, InterpPos};
use crate::manifest::{Component, ComponentKind, Manifest};
use crate::schema::{
    BuildConfig, CfgAtom, CfgPredicate, CfgTruth, DependencySpec, GenerateAction, GenerateStep,
    PackageFlags, Profile, ProjectFile, PublicHeaders, RunEntry, RuntimeData, TargetKind,
    TargetSpec, VisibilitySplit,
};
use crate::toolchain::{self, FlagClass, Lang};
use crate::Result;

#[derive(Debug, Clone)]
pub struct CompileUnit {
    pub source: PathBuf,
    pub lang: Lang,
    pub std: Option<u32>,
    /// (dir, is_system)
    pub includes: Vec<(PathBuf, bool)>,
    pub defines: Vec<(String, Option<String>)>,
    pub extra_flags: Vec<String>,
    /// Object path relative to the build dir (unique per target+source).
    pub object: PathBuf,
    /// True when this unit's source or any include dir lives under `${gen}`;
    /// ninja gives such units an order-only dep on the `cppkg-gen` phony.
    pub references_gen: bool,
}

#[derive(Debug, Clone)]
pub enum LinkInput {
    /// Objects of this target itself.
    Object(PathBuf),
    /// Static archive: absolute path for dep artifacts, build-dir-relative
    /// (same convention as `PlannedTarget::output`) for sibling target outs.
    Archive(PathBuf),
    Dylib(PathBuf),
    SystemLib(String),
    Framework(String),
    /// A raw link-flag word riding the ordered stream at its contributor's
    /// position (§1.3 interleaving). ninja emits it verbatim into $libs.
    Flag(String),
}

#[derive(Debug, Clone)]
pub struct PlannedTarget {
    pub name: String,
    pub kind: TargetKind,
    pub units: Vec<CompileUnit>,
    /// Output path relative to the build dir.
    pub output: PathBuf,
    /// Fully ordered link inputs (rule 5) — ninja_gen emits them verbatim.
    pub link_inputs: Vec<LinkInput>,
    pub link_flags: Vec<String>,
    pub link_lang: Lang,
    /// Sibling targets this one depends on (ninja dep edges).
    pub target_deps: Vec<String>,
    // -- export-facing effective (post-cfg, post-defaults) metadata so that
    // shim-export never re-evaluates cfg (§6.3/6.4 read these): -------------
    pub install: bool,
    pub dev: bool,
    pub test: bool,
    /// Absolute, cfg-projected public include dirs (incl. resolved ${gen}).
    pub public_includes: Vec<PathBuf>,
    pub public_defines: Vec<(String, Option<String>)>,
    /// cxx public flag bucket, merged order (uncond then matching cfg).
    pub public_flags: Vec<String>,
    pub public_link_flags: Vec<String>,
    /// Effective requirement: declared cxx-std max-merged with the closure's.
    pub cxx_std: Option<u32>,
    pub public_headers: Option<PublicHeaders>,
    /// Run entries with interpolation applied (test targets only).
    pub run: Vec<RunEntry>,
    /// External dep components in this target's link closure, by dep key —
    /// shim-export turns these into find_dependency/requires rows (§6.3).
    pub external_deps: BTreeMap<String, Vec<String>>,
    /// Direct sibling dependency edges by declared visibility (§6.3: public
    /// edges become `requires`, private ones `$<LINK_ONLY:...>` rows).
    pub local_deps_public: Vec<String>,
    pub local_deps_private: Vec<String>,
    /// Direct external component references (full exported names) by
    /// propagation class — same §6.3 split for external deps.
    pub external_public: Vec<String>,
    pub external_link_only: Vec<String>,
    /// Effective (cfg-projected) runtime-data declarations; the install
    /// side (§6.5) re-expands them under share/<package>/.
    pub runtime_data: Vec<RuntimeData>,
}

/// A [generate] step the requested set activates (§4.2 laziness). checked-in
/// steps NEVER appear here (outside the build graph; `cpp-pkg gen` owns them).
#[derive(Debug, Clone)]
pub struct PlannedGenStep {
    pub name: String,
    /// vars/argv/stdin/template already interpolated.
    pub action: GenerateAction,
    /// Declared inputs: tree inputs as written (project-root-relative);
    /// inputs that are other steps' outputs resolve to `<gen-root>/<rel>`.
    /// Existence of tree inputs is pre-checked (plan-time hard error).
    pub inputs: Vec<PathBuf>,
    /// Gen-root-relative output path.
    pub output: PathBuf,
}

/// One build-time runtime-data copy (§6.5), attached order-only to each
/// owning target's output edge by ninja.
#[derive(Debug, Clone)]
pub struct DataStage {
    /// Absolute source path.
    pub src: PathBuf,
    /// Build-dir-relative destination (beside the owning targets' outputs).
    pub dest: PathBuf,
    /// Targets whose build must stage this.
    pub for_targets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// Topological order (dependencies first).
    pub targets: Vec<PlannedTarget>,
    /// Activated generate steps, dependency order.
    pub gen_steps: Vec<PlannedGenStep>,
    /// Deduped build-time staging plan.
    pub data_stages: Vec<DataStage>,
    /// Non-fatal findings (negative globs matching nothing, ...).
    pub warnings: Vec<String>,
}

/// How the build set was requested (§3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildRequest {
    /// All non-dev targets (unmarked manifests: identical to v0).
    Default,
    /// Explicitly named targets (may include dev targets) + their transitive
    /// sibling closure.
    Named(Vec<String>),
    /// Test-marked targets matching the filters (substring match on the
    /// target name); empty = all tests.
    Tests(Vec<String>),
}

pub struct PlanInputs<'a> {
    pub project: &'a ProjectFile,
    pub project_root: &'a Path,
    /// Keyed by dependency key; only PROVISIONED deps are present.
    pub manifests: &'a BTreeMap<String, Manifest>,
    pub config: BuildConfig,
    pub profile: &'a Profile,
    /// Drives §2 cfg evaluation.
    pub cfg_truth: &'a CfgTruth,
    pub request: &'a BuildRequest,
    /// Pins, gen root, install-prefix — the §0.3 resolver context.
    pub interp: &'a InterpCtx<'a>,
}

/// Resolve + plan.
pub fn plan(inputs: &PlanInputs) -> Result<BuildPlan> {
    let project = inputs.project;
    let root = inputs.project_root;

    // One cfg projection, computed up front, read everywhere (§1.3: a cfg
    // entry lands in one and only one position in propagation AND emission).
    let mut effective: BTreeMap<String, EffectiveTarget> = BTreeMap::new();
    for (name, spec) in &project.targets {
        effective.insert(name.clone(), effective_target(spec, inputs.cfg_truth));
    }

    let gen_root_abs = resolve_gen_root(inputs.interp, root);

    let mut planner = Planner {
        project,
        root,
        manifests: inputs.manifests,
        config: inputs.config,
        profile: inputs.profile,
        truth: inputs.cfg_truth,
        ictx: inputs.interp,
        effective,
        gen_root_abs,
        gen_outputs: gen_output_table(&project.generate)?,
        exposed: BTreeMap::new(),
        resolve_cache: BTreeMap::new(),
        edge_cache: BTreeMap::new(),
        comp_reqs_cache: BTreeMap::new(),
        warnings: Vec::new(),
    };
    planner.build_exposed_table()?;

    let seeds = request_seeds(project, inputs.request)?;
    let selected = planner.select(&seeds)?;

    // Generate-step activation + ordering (§4.2) before units: units of
    // ${gen} sources validate against the declared-output table, and the
    // activation set is a pure function of the selected targets' references.
    let gen_steps = planner.plan_gen_steps(&selected, inputs.request)?;

    // Pass 1: compile units for every selected target. Link planning needs
    // sibling units to already exist (link-language rule inspects them), so
    // units are computed for the whole selection before any link plan.
    let mut units_map: BTreeMap<String, Vec<CompileUnit>> = BTreeMap::new();
    for name in &selected {
        let closure = planner.closure_nodes(name, false)?;
        let units = planner.compile_units(name, &closure)?;
        units_map.insert(name.clone(), units);
    }

    // Pass 2: link plans + export metadata + assembly.
    let mut planned: BTreeMap<String, PlannedTarget> = BTreeMap::new();
    for name in &selected {
        let spec = &project.targets[name];
        let units = units_map[name].clone();
        let edges = planner.edges(name)?;

        let mut target_deps: Vec<String> = Vec::new();
        for n in edges.public.iter().chain(edges.private.iter()) {
            if let Node::Sibling(s) = n
                && !target_deps.contains(s)
            {
                target_deps.push(s.clone());
            }
        }

        // Direct edges by visibility, for export emission (§6.3). Dedup
        // keep-first inside each bucket; the buckets may legitimately
        // overlap (a dep declared public and private is public).
        let mut local_deps_public: Vec<String> = Vec::new();
        let mut local_deps_private: Vec<String> = Vec::new();
        let mut external_public: Vec<String> = Vec::new();
        let mut external_link_only: Vec<String> = Vec::new();
        for (nodes, locals, externals) in [
            (&edges.public, &mut local_deps_public, &mut external_public),
            (&edges.private, &mut local_deps_private, &mut external_link_only),
        ] {
            for n in nodes {
                match n {
                    Node::Sibling(s) => {
                        if !locals.contains(s) {
                            locals.push(s.clone());
                        }
                    }
                    Node::Comp { name: comp, .. } => {
                        if !externals.contains(comp) {
                            externals.push(comp.clone());
                        }
                    }
                    // The builtin is emitted as a find_dependency(Threads)
                    // line by shim, keyed off external_deps mentions; it is
                    // no component reference.
                    Node::BuiltinThreads => {
                        let spelled = "Threads::Threads".to_string();
                        if !externals.contains(&spelled) {
                            externals.push(spelled);
                        }
                    }
                }
            }
        }

        // The link-language rule looks at the full any-edge closure: a
        // private (link-only) C++ archive still forces a C++ link.
        let any_closure = planner.closure_nodes(name, true)?;
        let link_lang = link_language(&units, &any_closure, &units_map);

        let (link_inputs, link_flags) = match spec.kind {
            // A static library does not link; its "link inputs" are the
            // objects the archiver packs. Its own link-flags propagate to
            // consumers' final links via the closure walk instead.
            TargetKind::StaticLibrary => (
                units
                    .iter()
                    .map(|u| LinkInput::Object(u.object.clone()))
                    .collect(),
                Vec::new(),
            ),
            TargetKind::Executable => planner.link_plan(name, &units)?,
        };

        let mut external_deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for n in &any_closure {
            if let Node::Comp { pkg, name } = n {
                external_deps
                    .entry(pkg.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }

        let eff = planner.effective[name].clone();
        let public_includes = planner.absolute_public_includes(name, &eff)?;
        let mut public_defines = Vec::new();
        for d in &eff.defines.public {
            public_defines.push(planner.interp_define(name, d)?);
        }
        let closure_std = planner.closure_cxx_std(&any_closure)?;
        let run = planner.interp_run_entries(name, &spec.run)?;

        planned.insert(
            name.clone(),
            PlannedTarget {
                name: name.clone(),
                kind: spec.kind,
                units,
                output: output_path(name, spec.kind),
                link_inputs,
                link_flags,
                link_lang,
                target_deps,
                install: spec.install,
                dev: is_dev(spec),
                test: spec.test,
                public_includes,
                public_defines,
                public_flags: eff.cxx_flags.public.clone(),
                public_link_flags: eff.link_flags.public.clone(),
                cxx_std: max_std(spec.cxx_std, closure_std),
                public_headers: spec.public_headers.clone(),
                run,
                external_deps: external_deps
                    .into_iter()
                    .map(|(k, v)| (k, v.into_iter().collect()))
                    .collect(),
                local_deps_public,
                local_deps_private,
                external_public,
                external_link_only,
                runtime_data: eff.runtime_data.clone(),
            },
        );
    }

    let data_stages = planner.plan_data_stages(&selected)?;

    let order = planner.topo(&selected)?;
    Ok(BuildPlan {
        targets: order
            .into_iter()
            .map(|n| planned.remove(&n).expect("topo order covers selection"))
            .collect(),
        gen_steps,
        data_stages,
        warnings: planner.warnings,
    })
}

/// Which dependency keys the request demands, BEFORE any fetch/probe
/// (provisioning laziness, §3.2 + §2.2 + §5.3):
/// - cfg-inactive deps are excluded always (locked, never provisioned);
/// - dev-deps are included only when the request selects a dev/test target;
/// - system deps are included only when the selection references them
///   attributably (depkey::/find-package prefix, exposes-* claims) or an
///   included dep `needs` them — an uninstalled sysdep must error at
///   need-time, never on unrelated builds.
///
/// Non-system fetched deps are otherwise always included (v0 behavior;
/// conservative superset sanctioned by the implementation plan).
pub fn required_deps(
    project: &ProjectFile,
    truth: &CfgTruth,
    request: &BuildRequest,
) -> Result<BTreeSet<String>> {
    let seeds = request_seeds(project, request)?;
    let selection = sibling_closure(project, truth, &seeds);
    let has_dev = selection
        .iter()
        .any(|t| is_dev(&project.targets[t]));

    let tables = project
        .dependencies
        .iter()
        .chain(project.dev_dependencies.iter());

    // Every dependency reference of the selection, for sysdep attribution.
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for t in &selection {
        let deps = effective_dependencies(&project.targets[t], truth);
        refs.extend(deps.public.iter().cloned());
        refs.extend(deps.private.iter().cloned());
    }

    let mut out: BTreeSet<String> = BTreeSet::new();
    for (key, spec) in tables.clone() {
        if !dep_active(spec, truth) {
            continue;
        }
        if spec.dev && !has_dev {
            continue;
        }
        if is_system_dep(spec) {
            if refs.iter().any(|r| attributable(r, key, spec)) {
                out.insert(key.clone());
            }
        } else {
            out.insert(key.clone());
        }
    }

    // `needs` edges: an included dep demands its needs. A need declared
    // behind a false cfg predicate is unbuildable — error naming it.
    // (System deps cannot have `needs`; schema rejects that.)
    let included: Vec<String> = out.iter().cloned().collect();
    for key in &included {
        let spec = project
            .dependencies
            .get(key)
            .or_else(|| project.dev_dependencies.get(key))
            .expect("included key comes from the tables");
        for need in &spec.needs {
            let need_spec = project
                .dependencies
                .get(need)
                .or_else(|| project.dev_dependencies.get(need));
            if let Some(ns) = need_spec {
                if !dep_active(ns, truth) {
                    bail!(
                        "dependency `{key}` needs `{need}`, which is declared \
                         behind cfg '{}' — false for this toolchain",
                        cfg_name(ns.cfg.as_ref().expect("inactive implies cfg"))
                    );
                }
                out.insert(need.clone());
            }
        }
    }
    Ok(out)
}

/// §0.4 pattern expansion with `!` negation: union of positives minus union
/// of negatives, applied after expansion. Positive literals must exist
/// (hard error); positive globs may match nothing (callers decide whether an
/// empty total is an error); a negative matching nothing pushes a warning
/// (upstream may have deleted the file — surface drift, don't break).
/// Expansion order is preserved (each glob sorted byte-wise, patterns in
/// declaration order, dedup keep-first) so v0 source ordering is unchanged.
pub fn expand_patterns(
    base: &Path,
    patterns: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<PathBuf>> {
    if !patterns.is_empty() && patterns.iter().all(|p| p.starts_with('!')) {
        // Schema rejects this upstream; defend anyway.
        bail!("pattern list contains only negative (`!`) patterns");
    }
    let base_str = base
        .to_str()
        .ok_or_else(|| anyhow!("base dir `{}` is not valid UTF-8", base.display()))?;

    let expand_one = |pat: &str| -> Result<Vec<PathBuf>> {
        // The glob crate's trailing `dir/**` matches directories only, never
        // the files below them; users write `!absl/testing/**` expecting
        // gitignore semantics. Normalize to `dir/**/*`, which matches files
        // at every depth (including directly under `dir`).
        let normalized;
        let pat = if pat.ends_with("/**") || pat == "**" {
            normalized = format!("{pat}/*");
            normalized.as_str()
        } else {
            pat
        };
        if pat.contains(['*', '?', '[']) {
            // Escape the base so glob metacharacters in the directory path
            // itself (e.g. brackets in a temp dir name) stay literal.
            let full = format!("{}/{}", glob::Pattern::escape(base_str), pat);
            let paths =
                glob::glob(&full).map_err(|e| anyhow!("invalid glob `{pat}`: {e}"))?;
            let mut matches: Vec<PathBuf> = Vec::new();
            for entry in paths {
                let p = entry.map_err(|e| anyhow!("glob `{pat}`: {e}"))?;
                if p.is_file() {
                    matches.push(p);
                }
            }
            matches.sort_by(|a, b| {
                a.as_os_str()
                    .as_encoded_bytes()
                    .cmp(b.as_os_str().as_encoded_bytes())
            });
            Ok(matches)
        } else {
            let p = base.join(pat);
            Ok(if p.is_file() { vec![p] } else { Vec::new() })
        }
    };

    let mut out: Vec<PathBuf> = Vec::new();
    for pat in patterns.iter().filter(|p| !p.starts_with('!')) {
        let matched = expand_one(pat)?;
        if matched.is_empty() && !pat.contains(['*', '?', '[']) {
            bail!("`{pat}` not found (looked at {})", base.join(pat).display());
        }
        out.extend(matched);
    }
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    out.retain(|p| seen.insert(p.clone()));

    let mut remove: BTreeSet<PathBuf> = BTreeSet::new();
    for pat in patterns.iter().filter(|p| p.starts_with('!')) {
        let matched = expand_one(&pat[1..])?;
        if matched.is_empty() {
            warnings.push(format!(
                "negative pattern `{pat}` (under {}) matched nothing — \
                 upstream may have removed the files it excluded",
                base.display()
            ));
        }
        remove.extend(matched);
    }
    out.retain(|p| !remove.contains(p));
    Ok(out)
}

// ---------------------------------------------------------------------------
// cfg projection + request selection (shared with required_deps)
// ---------------------------------------------------------------------------

/// The one effective (cfg-projected) view of a target: unconditional lists,
/// then each matching cfg group appended in document order, per key and per
/// visibility bucket (§0.5). Non-matching groups are never expanded.
#[derive(Debug, Clone, Default)]
struct EffectiveTarget {
    sources: Vec<String>,
    includes: VisibilitySplit,
    defines: VisibilitySplit,
    dependencies: VisibilitySplit,
    cxx_flags: VisibilitySplit,
    c_flags: VisibilitySplit,
    link_flags: VisibilitySplit,
    runtime_data: Vec<RuntimeData>,
    /// Names of non-matching cfg groups that carried sources — the
    /// zero-sources error names them so a wrong-platform build is legible.
    inactive_source_groups: Vec<String>,
}

fn append_split(dst: &mut VisibilitySplit, src: &VisibilitySplit) {
    dst.public.extend(src.public.iter().cloned());
    dst.private.extend(src.private.iter().cloned());
}

fn effective_target(spec: &TargetSpec, truth: &CfgTruth) -> EffectiveTarget {
    let mut eff = EffectiveTarget {
        sources: spec.sources.clone(),
        includes: spec.includes.clone(),
        defines: spec.defines.clone(),
        dependencies: spec.dependencies.clone(),
        cxx_flags: spec.cxx_flags.clone(),
        c_flags: spec.c_flags.clone(),
        link_flags: spec.link_flags.clone(),
        runtime_data: spec.runtime_data.clone(),
        inactive_source_groups: Vec::new(),
    };
    for (pred, group) in &spec.cfg {
        if pred.eval(truth) {
            eff.sources.extend(group.sources.iter().cloned());
            append_split(&mut eff.includes, &group.includes);
            append_split(&mut eff.defines, &group.defines);
            append_split(&mut eff.dependencies, &group.dependencies);
            append_split(&mut eff.cxx_flags, &group.cxx_flags);
            append_split(&mut eff.c_flags, &group.c_flags);
            append_split(&mut eff.link_flags, &group.link_flags);
            eff.runtime_data.extend(group.runtime_data.iter().cloned());
        } else if !group.sources.is_empty() {
            eff.inactive_source_groups.push(cfg_name(pred));
        }
    }
    eff
}

/// Effective dependency references only (manifest-free; used pre-fetch).
fn effective_dependencies(spec: &TargetSpec, truth: &CfgTruth) -> VisibilitySplit {
    let mut deps = spec.dependencies.clone();
    for (pred, group) in &spec.cfg {
        if pred.eval(truth) {
            append_split(&mut deps, &group.dependencies);
        }
    }
    deps
}

fn is_dev(spec: &TargetSpec) -> bool {
    // `test` implies `dev`; the loader sets both, but don't rely on it.
    spec.dev || spec.test
}

fn dep_active(spec: &DependencySpec, truth: &CfgTruth) -> bool {
    spec.cfg.as_ref().is_none_or(|p| p.eval(truth))
}

fn is_system_dep(spec: &DependencySpec) -> bool {
    matches!(spec.source, crate::schema::SourceSpec::System { .. })
}

/// Can `reference` be attributed to dependency `key` without its manifest?
/// (depkey prefix, find-package-name prefix, exposes-* declarations.)
fn attributable(reference: &str, key: &str, spec: &DependencySpec) -> bool {
    if reference == key {
        return true;
    }
    if let Some(ns) = namespace_of(reference)
        && (ns == key
            || spec.find_package.as_deref() == Some(ns)
            || spec.exposes_namespace.iter().any(|n| n == ns))
    {
        return true;
    }
    spec.exposes_targets.claims.iter().any(|c| c == reference)
        || spec.exposes_targets.renames.values().any(|v| v == reference)
}

fn cfg_name(pred: &CfgPredicate) -> String {
    match pred.atom {
        CfgAtom::Windows => "windows",
        CfgAtom::Macos => "macos",
        CfgAtom::Linux => "linux",
        CfgAtom::Unix => "unix",
        CfgAtom::Clang => "clang",
        CfgAtom::Gcc => "gcc",
        CfgAtom::Msvc => "msvc",
    }
    .to_string()
}

/// Seed targets for a request (§3.2). Default = all non-dev; Named must
/// exist; Tests = test targets matching the filters (substring), a filter
/// matching nothing is a hard error.
fn request_seeds(project: &ProjectFile, request: &BuildRequest) -> Result<Vec<String>> {
    match request {
        BuildRequest::Default => Ok(project
            .targets
            .iter()
            .filter(|(_, s)| !is_dev(s))
            .map(|(n, _)| n.clone())
            .collect()),
        BuildRequest::Named(names) => {
            for name in names {
                if !project.targets.contains_key(name) {
                    let known: Vec<&str> =
                        project.targets.keys().map(String::as_str).collect();
                    bail!(
                        "unknown target `{name}` (targets in this project: {})",
                        known.join(", ")
                    );
                }
            }
            Ok(names.clone())
        }
        BuildRequest::Tests(filters) => {
            let all_tests: Vec<String> = project
                .targets
                .iter()
                .filter(|(_, s)| s.test)
                .map(|(n, _)| n.clone())
                .collect();
            if filters.is_empty() {
                // Zero declared tests => empty plan; the cli prints
                // "no test targets" and exits 0 (§3.2).
                return Ok(all_tests);
            }
            let matched: Vec<String> = all_tests
                .iter()
                .filter(|t| filters.iter().any(|f| t.contains(f.as_str())))
                .cloned()
                .collect();
            if matched.is_empty() {
                if all_tests.is_empty() {
                    bail!(
                        "filter matches no tests: this project declares no \
                         `test = true` targets"
                    );
                }
                bail!(
                    "filter matches no tests (test targets: {})",
                    all_tests.join(", ")
                );
            }
            Ok(matched)
        }
    }
}

/// Manifest-free transitive sibling closure (references that are sibling
/// target names; everything else ignored). Used by required_deps, which
/// runs before any dependency is provisioned.
fn sibling_closure(
    project: &ProjectFile,
    truth: &CfgTruth,
    seeds: &[String],
) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    let mut work: Vec<String> = seeds.to_vec();
    while let Some(t) = work.pop() {
        if !project.targets.contains_key(&t) || !set.insert(t.clone()) {
            continue;
        }
        let deps = effective_dependencies(&project.targets[&t], truth);
        for r in deps.public.iter().chain(deps.private.iter()) {
            if project.targets.contains_key(r) {
                work.push(r.clone());
            }
        }
    }
    set
}

// ---------------------------------------------------------------------------
// Internal machinery
// ---------------------------------------------------------------------------

/// A node in the dependency graph: a sibling target of this project, a
/// component owned by a dependency package (post-attribution), or a builtin
/// pseudo-package (ladder step 0).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Sibling(String),
    Comp { pkg: String, name: String },
    /// `Threads::Threads` (§5.4): expansion is a pure function of the
    /// toolchain os axis; contributes flags, never artifacts.
    BuiltinThreads,
}

impl Node {
    fn describe(&self) -> String {
        match self {
            Node::Sibling(s) => format!("{s} (project target)"),
            Node::Comp { pkg, name } => format!("{name} (package {pkg})"),
            Node::BuiltinThreads => "Threads::Threads (builtin)".to_string(),
        }
    }
}

/// Resolved direct dependency edges of one target.
#[derive(Debug, Clone, Default)]
struct Edges {
    public: Vec<Node>,
    private: Vec<Node>,
}

/// Accumulated compile-side usage requirements.
#[derive(Debug, Clone, Default)]
struct Reqs {
    includes: Vec<(PathBuf, bool)>,
    defines: Vec<(String, Option<String>)>,
    options: Vec<String>,
    cxx_std: Option<u32>,
}

impl Reqs {
    /// Keep-first dedup; includes dedup by directory (first spelling wins,
    /// including its system-ness).
    fn dedup(&mut self) {
        let mut seen_inc: BTreeSet<PathBuf> = BTreeSet::new();
        self.includes.retain(|(p, _)| seen_inc.insert(p.clone()));
        let mut seen_def: BTreeSet<(String, Option<String>)> = BTreeSet::new();
        self.defines.retain(|d| seen_def.insert(d.clone()));
        let mut seen_opt: BTreeSet<String> = BTreeSet::new();
        self.options.retain(|o| seen_opt.insert(o.clone()));
    }
}

struct Planner<'a> {
    project: &'a ProjectFile,
    root: &'a Path,
    manifests: &'a BTreeMap<String, Manifest>,
    config: BuildConfig,
    profile: &'a Profile,
    truth: &'a CfgTruth,
    ictx: &'a InterpCtx<'a>,
    /// One cfg projection per target (see `effective_target`).
    effective: BTreeMap<String, EffectiveTarget>,
    /// Absolute ${gen} root.
    gen_root_abs: PathBuf,
    /// Declared step outputs: rel path -> (step name, is_checked_in).
    gen_outputs: BTreeMap<PathBuf, (String, bool)>,
    /// exposed name -> [(dep key, extracted component name)]
    exposed: BTreeMap<String, Vec<(String, String)>>,
    resolve_cache: BTreeMap<String, Node>,
    edge_cache: BTreeMap<String, Edges>,
    comp_reqs_cache: BTreeMap<(String, String), Reqs>,
    warnings: Vec<String>,
}

/// CMake spelling of the config; keys `Component::location`.
fn cmake_config_name(config: BuildConfig) -> &'static str {
    config.cmake_name()
}

fn namespace_of(name: &str) -> Option<&str> {
    name.split_once("::").map(|(ns, _)| ns)
}

fn parse_define(s: &str) -> (String, Option<String>) {
    match s.split_once('=') {
        Some((k, v)) => (k.to_string(), Some(v.to_string())),
        None => (s.to_string(), None),
    }
}

fn max_std(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn output_path(name: &str, kind: TargetKind) -> PathBuf {
    match kind {
        TargetKind::Executable => PathBuf::from(name),
        TargetKind::StaticLibrary => PathBuf::from(format!("lib{name}.a")),
    }
}

/// Classify a source file by extension. Exhaustive table; everything else is
/// a hard error (never silently treated as C++).
fn classify_source(path: &Path) -> Result<Lang> {
    let ext = path.extension().and_then(|e| e.to_str());
    match ext {
        Some("cpp") | Some("cc") | Some("cxx") | Some("c++") => Ok(Lang::Cxx),
        Some("c") => Ok(Lang::C),
        Some("C") => bail!(
            "source `{}` uses extension `.C`: indistinguishable from `.c` on \
             case-insensitive filesystems (the macOS default); rename it to \
             `.cpp` (or `.c` if it is C)",
            path.display()
        ),
        Some("m") | Some("mm") => bail!(
            "source `{}`: Objective-C not supported in v0",
            path.display()
        ),
        Some(other) => bail!(
            "source `{}` has unknown extension `.{other}`; recognized \
             extensions: .cpp .cc .cxx .c++ (C++), .c (C)",
            path.display()
        ),
        None => bail!(
            "source `{}` has no extension; recognized extensions: \
             .cpp .cc .cxx .c++ (C++), .c (C)",
            path.display()
        ),
    }
}

/// Object path relative to the build dir: `<target>.dir/<relpath>.o`, unique
/// per (target, source). The `.dir` suffix (CMake's convention) keeps the
/// object tree from colliding with the target's own output. Sources outside
/// the project root (interface sources living in the store) get a path-hash
/// prefix instead of a relpath so two distinct external sources with the
/// same file name cannot collide.
fn object_path(target: &str, root: &Path, src: &Path) -> PathBuf {
    let obj_root = PathBuf::from(format!("{target}.dir"));
    // The lexical strip only helps when the result stays inside the object
    // tree: `..`/`.` segments take the hashed branch instead.
    if let Ok(rel) = src.strip_prefix(root)
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        let mut s = rel.to_string_lossy().into_owned();
        s.push_str(".o");
        return obj_root.join(s);
    }
    let hash = blake3::hash(src.as_os_str().as_encoded_bytes());
    let hex = hash.to_hex();
    let short = &hex.as_str()[..8];
    let fname = src
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".to_string());
    obj_root.join("ext").join(format!("{short}-{fname}.o"))
}

fn link_language(
    units: &[CompileUnit],
    closure: &[Node],
    units_map: &BTreeMap<String, Vec<CompileUnit>>,
) -> Lang {
    if units.iter().any(|u| u.lang == Lang::Cxx) {
        return Lang::Cxx;
    }
    for n in closure {
        match n {
            Node::Sibling(s) => {
                if units_map
                    .get(s)
                    .is_some_and(|us| us.iter().any(|u| u.lang == Lang::Cxx))
                {
                    return Lang::Cxx;
                }
            }
            // A dependency component's implementation language is unknown to
            // us; assume C++. Linking pure C with the C++ driver is harmless,
            // while the reverse would drop the C++ runtime.
            Node::Comp { .. } => return Lang::Cxx,
            Node::BuiltinThreads => {}
        }
    }
    Lang::C
}

/// Guess an artifact kind from its file extension (used for UNKNOWN imported
/// types and raw link paths).
fn classify_artifact(path: PathBuf) -> LinkInput {
    match path.extension().and_then(|e| e.to_str()) {
        Some("dylib") | Some("so") | Some("tbd") => LinkInput::Dylib(path),
        _ => LinkInput::Archive(path),
    }
}

/// One raw link-stream entry plus, for Flag entries, the identity that
/// dedup keys on: (contributor, occurrence-within-that-contributor's-block).
struct RawLink {
    input: LinkInput,
    flag_key: Option<(String, usize)>,
}

/// Rule-5 dedup: archives keep the LAST occurrence (symbol resolution walks
/// left to right; the last position satisfies every earlier referencer);
/// Flag words keep the last occurrence of their (contributor, occurrence)
/// key so they stay glued to the kept archive (§1.3 interleaving through
/// diamonds); everything else keeps the first.
fn dedup_link_inputs(raw: Vec<RawLink>) -> Vec<LinkInput> {
    let mut last_archive: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut last_flag: BTreeMap<(String, usize), usize> = BTreeMap::new();
    for (i, rl) in raw.iter().enumerate() {
        match &rl.input {
            LinkInput::Archive(p) => {
                last_archive.insert(p.clone(), i);
            }
            LinkInput::Flag(_) => {
                if let Some(k) = &rl.flag_key {
                    last_flag.insert(k.clone(), i);
                }
            }
            _ => {}
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for (i, rl) in raw.into_iter().enumerate() {
        let keep = match &rl.input {
            LinkInput::Archive(p) => last_archive[p] == i,
            LinkInput::Flag(_) => match &rl.flag_key {
                Some(k) => last_flag[k] == i,
                None => true,
            },
            LinkInput::Object(p) => seen.insert(format!("obj\u{1f}{}", p.display())),
            LinkInput::Dylib(p) => seen.insert(format!("dylib\u{1f}{}", p.display())),
            LinkInput::SystemLib(n) => seen.insert(format!("sys\u{1f}{n}")),
            LinkInput::Framework(n) => seen.insert(format!("fw\u{1f}{n}")),
        };
        if keep {
            out.push(rl.input);
        }
    }
    out
}

/// Strip ABI-classified words from a `[flags]` list (§1.3 layer 3 carries
/// the NON-ABI remainder; the cli injects ABI words at layer 2 and folds
/// them into dependency config hashes). Two-argv forms drop both words.
fn strip_abi_words(words: Vec<String>) -> Vec<String> {
    if words.is_empty() {
        return words;
    }
    let classified = toolchain::classify_word_sequence(&words);
    let mut drop = vec![false; words.len()];
    for c in &classified {
        if matches!(c.class, FlagClass::Abi) && c.index < drop.len() {
            drop[c.index] = true;
        }
    }
    words
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(_, w)| w)
        .collect()
}

/// `[flags]` contribution for a compile line (§1.3 step 3): unconditional
/// entries, then matching cfg groups in document order, ABI words stripped.
fn package_compile_flags(flags: &PackageFlags, truth: &CfgTruth, lang: Lang) -> Vec<String> {
    let pick = |g_cxx: &Vec<String>, g_c: &Vec<String>| match lang {
        Lang::Cxx => g_cxx.clone(),
        Lang::C => g_c.clone(),
    };
    let mut words = pick(&flags.cxx_flags, &flags.c_flags);
    for (pred, group) in &flags.cfg {
        if pred.eval(truth) {
            words.extend(pick(&group.cxx_flags, &group.c_flags));
        }
    }
    strip_abi_words(words)
}

/// `[flags]` link contribution (same shape, link list).
fn package_link_flags(flags: &PackageFlags, truth: &CfgTruth) -> Vec<String> {
    let mut words = flags.link_flags.clone();
    for (pred, group) in &flags.cfg {
        if pred.eval(truth) {
            words.extend(group.link_flags.iter().cloned());
        }
    }
    strip_abi_words(words)
}

/// Absolute ${gen} root: the interp context's gen root, absolutized against
/// the project root (the context may carry it project-root-relative).
fn resolve_gen_root(ictx: &InterpCtx, root: &Path) -> PathBuf {
    match ictx.gen_root {
        Some(g) if g.is_absolute() => g.to_path_buf(),
        Some(g) => root.join(g),
        // No gen root in context: derive the spec default (build/gen). Only
        // reachable when the cli passes no context value; ${gen} references
        // still resolve consistently.
        None => root.join("build").join("gen"),
    }
}

/// Declared output table: gen-root-relative path -> (step, checked-in?).
/// (Case-insensitive collision checking is schema's; this table is keyed by
/// the declared spelling.)
fn gen_output_table(
    steps: &BTreeMap<String, GenerateStep>,
) -> Result<BTreeMap<PathBuf, (String, bool)>> {
    let mut out = BTreeMap::new();
    for (name, step) in steps {
        let rel = gen_output_rel(&step.action);
        out.insert(rel, (name.clone(), step.checked_in.is_some()));
    }
    Ok(out)
}

fn gen_output_rel(action: &GenerateAction) -> PathBuf {
    match action {
        GenerateAction::Template { output, .. } => PathBuf::from(output),
        GenerateAction::Command { stdout, .. } => PathBuf::from(stdout),
    }
}

/// Split a raw manifest string entry on its `${gen}` root. Returns:
/// - `Ok(Some(rel))` — entry is `${gen}` or `${gen}/<rel>`;
/// - `Ok(None)` — no ${gen} reference;
/// - `Err` — ${gen} appears somewhere other than the start (a generated
///   path must be gen-rooted).
fn gen_rel_of(entry: &str) -> Result<Option<String>> {
    if let Some(rest) = entry.strip_prefix("${gen}") {
        if rest.is_empty() {
            return Ok(Some(String::new()));
        }
        if let Some(rel) = rest.strip_prefix('/') {
            if rel.contains("${") {
                bail!("`{entry}`: only a single leading ${{gen}} is supported here");
            }
            return Ok(Some(rel.to_string()));
        }
        bail!("`{entry}`: ${{gen}} must be followed by `/` or end the entry");
    }
    // "$${gen}..." is an escaped literal, not a reference.
    Ok(None)
}

/// Extract `${gen}`-rooted prefixes from a run-entry value for step
/// activation ("--data=${gen}/x" activates steps under x/; a bare `${gen}`
/// activates everything).
fn gen_prefixes_in(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while let Some(pos) = raw[i..].find("${gen}") {
        let at = i + pos;
        // "$${gen}" is an escaped literal `${gen}`.
        if at > 0 && bytes[at - 1] == b'$' {
            i = at + 6;
            continue;
        }
        let rest = &raw[at + 6..];
        if let Some(tail) = rest.strip_prefix('/') {
            let end = tail.find("${").unwrap_or(tail.len());
            out.push(tail[..end].to_string());
        } else {
            out.push(String::new());
        }
        i = at + 6;
    }
    out
}

impl<'a> Planner<'a> {
    fn component(&self, pkg: &str, name: &str) -> Result<&'a Component> {
        let manifests: &'a BTreeMap<String, Manifest> = self.manifests;
        manifests
            .get(pkg)
            .and_then(|m| m.components.get(name))
            .ok_or_else(|| anyhow!("internal error: unresolved component `{name}` of `{pkg}`"))
    }

    /// Dependency spec by key, across both tables (§3.2: one namespace).
    fn dep_spec(&self, key: &str) -> Option<&'a DependencySpec> {
        let project: &'a ProjectFile = self.project;
        project
            .dependencies
            .get(key)
            .or_else(|| project.dev_dependencies.get(key))
    }

    /// Exposed-name table: every manifest component under its exposed name
    /// (the `exposes-targets` mapping form renames — the extracted name is
    /// then no longer exposed, which is itself a disambiguation mechanism).
    fn build_exposed_table(&mut self) -> Result<()> {
        for (key, manifest) in self.manifests {
            let renames = self.dep_spec(key).map(|d| &d.exposes_targets.renames);
            for comp_name in manifest.components.keys() {
                let renamed = renames.and_then(|r| r.get(comp_name)).cloned();
                // Project target names always win in `resolve`, so a rename
                // targeting one would be silently unreachable — reject it
                // rather than let every reference bind to the sibling.
                if let Some(exposed) = &renamed
                    && self.project.targets.contains_key(exposed)
                {
                    bail!(
                        "dependency `{key}`: exposes-targets renames \
                         `{comp_name}` to `{exposed}`, which is already a \
                         target of this project; project target names take \
                         precedence, so the rename could never be referenced \
                         — pick a different exposed name"
                    );
                }
                let exposed = renamed.unwrap_or_else(|| comp_name.clone());
                let entry = self.exposed.entry(exposed.clone()).or_default();
                if entry.iter().any(|(k, _)| k == key) {
                    bail!(
                        "dependency `{key}` exposes `{exposed}` more than once \
                         (an exposes-targets rename collides with another \
                         exported target)"
                    );
                }
                entry.push((key.clone(), comp_name.clone()));
            }
        }
        Ok(())
    }

    /// The naming ladder (CPPKG_TOML.md "Target references"). `context` is
    /// interpolated into error messages ("referenced by target `x`").
    fn resolve(&mut self, reference: &str, context: &str) -> Result<Node> {
        if let Some(n) = self.resolve_cache.get(reference) {
            return Ok(n.clone());
        }
        let node = self.resolve_uncached(reference, context)?;
        self.resolve_cache.insert(reference.to_string(), node.clone());
        Ok(node)
    }

    fn resolve_uncached(&mut self, reference: &str, context: &str) -> Result<Node> {
        // Ladder step 0 (§0.6): builtin pseudo-packages resolve first and
        // cannot be shadowed. `builtin:threads` is the extraction-side
        // symbolic spelling of the same node (§5.4).
        if reference == "Threads::Threads" || reference == "builtin:threads" {
            return Ok(Node::BuiltinThreads);
        }
        // Local target names (charset excludes "::") resolve to siblings.
        if self.project.targets.contains_key(reference) {
            return Ok(Node::Sibling(reference.to_string()));
        }
        let candidates: Vec<(String, String)> =
            self.exposed.get(reference).cloned().unwrap_or_default();

        // Ladder step 1: unique across all dependencies' manifests.
        if candidates.len() == 1 {
            let (pkg, name) = candidates.into_iter().next().unwrap();
            return Ok(Node::Comp { pkg, name });
        }

        // Ladder step 2: a `<depkey>::` prefix names the owning dependency
        // DECISIVELY — either that dependency exports the name, or the
        // reference is an error against that dependency. It never falls
        // through to exposes-* claims by other packages.
        if let Some(prefix) = namespace_of(reference)
            && (self.project.dependencies.contains_key(prefix)
                || self.project.dev_dependencies.contains_key(prefix)
                || self.manifests.contains_key(prefix))
        {
            if let Some((pkg, name)) = candidates.iter().find(|(k, _)| k == prefix) {
                return Ok(Node::Comp {
                    pkg: pkg.clone(),
                    name: name.clone(),
                });
            }
            // The prefix names a dependency that is declared but not
            // provisioned: explain WHY it is absent before claiming it
            // exports nothing.
            if !self.manifests.contains_key(prefix)
                && let Some(spec) = self.dep_spec(prefix)
            {
                return Err(self.unprovisioned_error(reference, context, prefix, spec));
            }
            let known: Vec<String> = self
                .exposed
                .iter()
                .filter(|(_, owners)| owners.iter().any(|(k, _)| k == prefix))
                .map(|(n, _)| n.clone())
                .collect();
            bail!(
                "dependency `{prefix}` does not export target \
                 `{reference}` ({context}); it exports: {}",
                if known.is_empty() {
                    "nothing".to_string()
                } else {
                    known.join(", ")
                }
            );
        }

        if candidates.is_empty() {
            // §2.2/§3.2: attribute the reference to a declared-but-absent
            // dependency where possible, so the error names the real cause
            // (false cfg predicate / unprovisioned dev-dep or sysdep)
            // instead of a bare "unknown reference".
            for (key, spec) in self
                .project
                .dependencies
                .iter()
                .chain(self.project.dev_dependencies.iter())
            {
                if !self.manifests.contains_key(key) && attributable(reference, key, spec) {
                    return Err(self.unprovisioned_error(reference, context, key, spec));
                }
            }
            bail!(
                "unknown dependency reference `{reference}` ({context}): not a \
                 target of this project and not exported by any dependency"
            );
        }

        // Ladder step 3: exposes-namespace / exposes-targets declarations.
        let claimed: Vec<(String, String)> = candidates
            .iter()
            .filter(|(pkg, extracted)| {
                let Some(dep) = self.dep_spec(pkg) else {
                    return false;
                };
                dep.exposes_targets.claims.iter().any(|c| c == extracted)
                    || dep.exposes_targets.renames.contains_key(extracted)
                    || namespace_of(extracted)
                        .is_some_and(|ns| dep.exposes_namespace.iter().any(|n| n == ns))
            })
            .cloned()
            .collect();
        match claimed.len() {
            1 => {
                let (pkg, name) = claimed.into_iter().next().unwrap();
                return Ok(Node::Comp { pkg, name });
            }
            0 => {}
            _ => {
                let keys: Vec<&str> = claimed.iter().map(|(k, _)| k.as_str()).collect();
                bail!(
                    "conflicting exposes-* declarations for `{reference}` \
                     ({context}): claimed by dependencies {}",
                    keys.join(", ")
                );
            }
        }

        // Ladder step 4: hard error, never first-wins. List every candidate
        // owner and the exposes-* addition that would disambiguate.
        let keys: Vec<&str> = candidates.iter().map(|(k, _)| k.as_str()).collect();
        let ns_hint = namespace_of(reference)
            .map(|ns| format!("`exposes-namespace = [\"{ns}\"]` or "))
            .unwrap_or_default();
        bail!(
            "ambiguous target reference `{reference}` ({context}): exported by \
             dependencies {}; add {ns_hint}`exposes-targets = [\"{reference}\"]` \
             to the [dependencies.<key>] entry of the package that owns it",
            keys.join(", ")
        );
    }

    /// The reference points at a dependency that is declared but has no
    /// manifest in this plan: say why (§2.2 cfg'd out / §3.2 lazy dev-dep /
    /// §5.3 lazy sysdep).
    fn unprovisioned_error(
        &self,
        reference: &str,
        context: &str,
        key: &str,
        spec: &DependencySpec,
    ) -> anyhow::Error {
        if let Some(pred) = &spec.cfg
            && !pred.eval(self.truth)
        {
            return anyhow!(
                "unresolved reference `{reference}` ({context}): dependency \
                 `{key}` is declared behind cfg '{}', which is false for this \
                 toolchain",
                cfg_name(pred)
            );
        }
        if spec.dev {
            return anyhow!(
                "unresolved reference `{reference}` ({context}): `{key}` is a \
                 dev-dependency, which is provisioned only when a dev/test \
                 target is requested; dev-dep targets are reachable only from \
                 dev/test targets (§dev graph)"
            );
        }
        if is_system_dep(spec) {
            return anyhow!(
                "unresolved reference `{reference}` ({context}): system \
                 dependency `{key}` was not provisioned for this build; if \
                 this reference belongs to it, add `exposes-targets = \
                 [\"{reference}\"]` (or exposes-namespace) to \
                 [dependencies.{key}] so cpp-pkg knows to probe it"
            );
        }
        anyhow!(
            "unresolved reference `{reference}` ({context}): dependency \
             `{key}` has no manifest in this plan (internal provisioning gap)"
        )
    }

    /// Resolve one target's direct dependency edges (cached), from the
    /// EFFECTIVE (cfg-projected) dependency lists, enforcing the §3.2
    /// dev-edge rule at the edge's origin.
    fn edges(&mut self, tname: &str) -> Result<Edges> {
        if let Some(e) = self.edge_cache.get(tname) {
            return Ok(e.clone());
        }
        let project: &'a ProjectFile = self.project;
        let spec = project
            .targets
            .get(tname)
            .ok_or_else(|| anyhow!("unknown target `{tname}`"))?;
        let self_dev = is_dev(spec);
        let deps = self.effective[tname].dependencies.clone();
        let context = format!("referenced by target `{tname}`");
        let mut edges = Edges::default();
        for (names, out) in [
            (&deps.public, &mut edges.public),
            (&deps.private, &mut edges.private),
        ] {
            for r in names {
                let node = self.resolve(r, &context)?;
                match &node {
                    Node::Sibling(s) => {
                        let dep_spec = &project.targets[s];
                        if dep_spec.kind == TargetKind::Executable {
                            bail!("target `{tname}` depends on `{s}`, which is an executable");
                        }
                        if !self_dev && is_dev(dep_spec) {
                            bail!(
                                "target '{tname}' (not a dev target) depends on \
                                 dev target '{s}'\nhint: mark '{tname}' \
                                 dev/test, or remove the dev marker from '{s}'"
                            );
                        }
                    }
                    Node::Comp { pkg, .. } => {
                        if !self_dev
                            && self.dep_spec(pkg).is_some_and(|d| d.dev)
                        {
                            bail!(
                                "target '{tname}' (not a dev target) depends on \
                                 '{r}', exported by dev-dependency '{pkg}'\n\
                                 hint: mark the target dev/test, or move \
                                 '{pkg}' to [dependencies]"
                            );
                        }
                    }
                    Node::BuiltinThreads => {}
                }
                out.push(node);
            }
        }
        self.edge_cache.insert(tname.to_string(), edges.clone());
        Ok(edges)
    }

    /// Selection: seed targets plus their transitive sibling dependencies
    /// (all visibilities — a private sibling still has to be built).
    fn select(&mut self, seeds: &[String]) -> Result<BTreeSet<String>> {
        let mut work: Vec<String> = seeds.to_vec();
        let mut set = BTreeSet::new();
        while let Some(t) = work.pop() {
            if !set.insert(t.clone()) {
                continue;
            }
            let e = self.edges(&t)?;
            for n in e.public.iter().chain(e.private.iter()) {
                if let Node::Sibling(s) = n {
                    work.push(s.clone());
                }
            }
        }
        Ok(set)
    }

    /// Closure of nodes reachable from `tname`, in first-reach DFS order.
    /// `all_edges = false` follows compile propagation (direct edges of any
    /// visibility, then PUBLIC-only continuation: sibling public deps,
    /// component `requires`); `all_edges = true` follows everything including
    /// link-only edges (sibling private deps, component `link_requires`).
    fn closure_nodes(&mut self, tname: &str, all_edges: bool) -> Result<Vec<Node>> {
        let e = self.edges(tname)?;
        let mut order = Vec::new();
        let mut seen = BTreeSet::new();
        for n in e.public.iter().chain(e.private.iter()) {
            self.closure_visit(n, all_edges, &mut order, &mut seen)?;
        }
        Ok(order)
    }

    fn closure_visit(
        &mut self,
        node: &Node,
        all_edges: bool,
        order: &mut Vec<Node>,
        seen: &mut BTreeSet<Node>,
    ) -> Result<()> {
        if !seen.insert(node.clone()) {
            return Ok(());
        }
        order.push(node.clone());
        let children: Vec<Node> = match node {
            Node::Sibling(s) => {
                let e = self.edges(s)?;
                if all_edges {
                    e.public.into_iter().chain(e.private).collect()
                } else {
                    e.public
                }
            }
            Node::Comp { pkg, name } => {
                let comp = self.component(pkg, name)?;
                let context = format!("required by component `{name}` of dependency `{pkg}`");
                let mut refs: Vec<&'a String> = comp.requires.iter().collect();
                if all_edges {
                    refs.extend(comp.link_requires.iter());
                }
                let mut out = Vec::new();
                for r in refs {
                    let child = self.resolve(r, &context)?;
                    if matches!(child, Node::Sibling(_)) {
                        bail!(
                            "component `{name}` of dependency `{pkg}` requires \
                             `{r}`, which is a target of this project"
                        );
                    }
                    out.push(child);
                }
                out
            }
            Node::BuiltinThreads => Vec::new(),
        };
        for child in children {
            self.closure_visit(&child, all_edges, order, seen)?;
        }
        Ok(())
    }

    /// Full compile requirements of one component: its own usage
    /// requirements plus (recursively) those of its public `requires`.
    /// Cycles among `requires` are a hard error.
    fn comp_reqs(
        &mut self,
        pkg: &str,
        name: &str,
        stack: &mut Vec<(String, String)>,
    ) -> Result<Reqs> {
        let key = (pkg.to_string(), name.to_string());
        if let Some(r) = self.comp_reqs_cache.get(&key) {
            return Ok(r.clone());
        }
        if stack.contains(&key) {
            let mut cycle: Vec<String> = stack
                .iter()
                .skip_while(|k| **k != key)
                .map(|(p, n)| format!("{n} ({p})"))
                .collect();
            cycle.push(format!("{name} ({pkg})"));
            bail!("dependency cycle among components: {}", cycle.join(" -> "));
        }
        stack.push(key.clone());
        let comp = self.component(pkg, name)?;
        let force_i = self.dep_system_includes_off(pkg);
        let mut reqs = Reqs::default();
        for i in &comp.includes {
            reqs.includes.push((i.clone(), false));
        }
        for i in &comp.system_includes {
            reqs.includes.push((i.clone(), !force_i));
        }
        reqs.defines.extend(comp.defines.iter().cloned());
        reqs.options.extend(comp.compile_options.iter().cloned());
        reqs.cxx_std = comp.cxx_std;
        let context = format!("required by component `{name}` of dependency `{pkg}`");
        for r in &comp.requires {
            let child = self.resolve(r, &context)?;
            match child {
                Node::Comp { pkg: cp, name: cn } => {
                    let sub = self.comp_reqs(&cp, &cn, stack)?;
                    reqs.includes.extend(sub.includes);
                    reqs.defines.extend(sub.defines);
                    reqs.options.extend(sub.options);
                    reqs.cxx_std = max_std(reqs.cxx_std, sub.cxx_std);
                }
                Node::BuiltinThreads => {
                    let (compile, _link) = toolchain::threads_expansion(self.truth.os);
                    reqs.options.extend(compile.iter().map(|s| s.to_string()));
                }
                Node::Sibling(_) => bail!(
                    "component `{name}` of dependency `{pkg}` requires `{r}`, \
                     which is a target of this project"
                ),
            }
        }
        stack.pop();
        self.comp_reqs_cache.insert(key, reqs.clone());
        Ok(reqs)
    }

    /// `system-includes = false` on the dependency key opts this dep's
    /// headers out of -isystem (§1.1); default (None/true) keeps the
    /// manifest's classification.
    fn dep_system_includes_off(&self, pkg: &str) -> bool {
        self.dep_spec(pkg)
            .is_some_and(|d| d.system_includes == Some(false))
    }

    /// Max cxx-std demanded by dependency components in a closure.
    fn closure_cxx_std(&mut self, closure: &[Node]) -> Result<Option<u32>> {
        let mut std = None;
        for n in closure {
            if let Node::Comp { pkg, name } = n {
                std = max_std(std, self.component(pkg, name)?.cxx_std);
            }
        }
        Ok(std)
    }

    fn interp_define(&self, tname: &str, raw: &str) -> Result<(String, Option<String>)> {
        let s = interp::interpolate(raw, InterpPos::DefineValue, self.ictx)
            .map_err(|e| anyhow!("target `{tname}`: in define `{raw}`: {e}"))?;
        Ok(parse_define(&s))
    }

    /// Resolve one include entry: `${gen}`-rooted entries land under the gen
    /// root (always -I, never -isystem: your generated code is your code);
    /// plain entries interpolate (escape handling / stray-var errors) and
    /// join the project root.
    fn resolve_include_entry(
        &self,
        tname: &str,
        entry: &str,
    ) -> Result<(PathBuf, /*is_gen*/ bool)> {
        if let Some(rel) = gen_rel_of(entry)
            .map_err(|e| anyhow!("target `{tname}`: include {e}"))?
        {
            let dir = if rel.is_empty() {
                self.gen_root_abs.clone()
            } else {
                self.gen_root_abs.join(rel)
            };
            return Ok((dir, true));
        }
        let s = interp::interpolate(entry, InterpPos::SourceOrIncludeEntry, self.ictx)
            .map_err(|e| anyhow!("target `{tname}`: in include `{entry}`: {e}"))?;
        Ok((self.root.join(s), false))
    }

    /// Run entries with §0.3 interpolation applied to args/cwd/env values.
    fn interp_run_entries(&self, tname: &str, entries: &[RunEntry]) -> Result<Vec<RunEntry>> {
        let mut out = Vec::new();
        for e in entries {
            let ictx = self.ictx;
            let one = |v: &str| -> Result<String> {
                interp::interpolate(v, InterpPos::RunEntryValue, ictx)
                    .map_err(|err| anyhow!("target `{tname}`: in run entry value `{v}`: {err}"))
            };
            let mut args = Vec::new();
            for a in &e.args {
                args.push(one(a)?);
            }
            let cwd = match &e.cwd {
                Some(c) => Some(one(c)?),
                None => None,
            };
            let mut env = BTreeMap::new();
            for (k, v) in &e.env {
                env.insert(k.clone(), one(v)?);
            }
            out.push(RunEntry {
                name: e.name.clone(),
                args,
                cwd,
                env,
                env_remove: e.env_remove.clone(),
                expect_failure: e.expect_failure,
            });
        }
        Ok(out)
    }

    /// Absolute cfg-projected public include dirs for export metadata.
    fn absolute_public_includes(
        &self,
        tname: &str,
        eff: &EffectiveTarget,
    ) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for inc in &eff.includes.public {
            let (dir, _) = self.resolve_include_entry(tname, inc)?;
            out.push(dir);
        }
        Ok(out)
    }

    /// Compile units of one target: its own sources (globs expanded with
    /// `!` negation, ${gen} sources validated against declared outputs)
    /// plus the interface sources of every component in its compile closure.
    fn compile_units(&mut self, tname: &str, closure: &[Node]) -> Result<Vec<CompileUnit>> {
        let project: &'a ProjectFile = self.project;
        let spec = &project.targets[tname];
        let eff = self.effective[tname].clone();

        // Partition sources: ${gen} entries resolve against the declared
        // output table (no globbing under the gen root); the rest expand
        // on disk with negation.
        let mut disk_patterns: Vec<String> = Vec::new();
        let mut gen_sources: Vec<PathBuf> = Vec::new();
        for entry in &eff.sources {
            match gen_rel_of(entry)
                .map_err(|e| anyhow!("target `{tname}`: source {e}"))?
            {
                Some(rel) => {
                    if rel.contains(['*', '?', '[']) {
                        bail!(
                            "target `{tname}`: source `{entry}`: no globbing \
                             under ${{gen}} — a generated source must name a \
                             declared step output exactly"
                        );
                    }
                    let rel_path = PathBuf::from(&rel);
                    match self.gen_outputs.get(&rel_path) {
                        Some((_, false)) => gen_sources.push(self.gen_root_abs.join(rel_path)),
                        Some((step, true)) => {
                            let checked = project.generate[step]
                                .checked_in
                                .clone()
                                .unwrap_or_default();
                            bail!(
                                "target `{tname}`: source `{entry}` names the \
                                 output of checked-in step `{step}`; the build \
                                 compiles the committed file — reference \
                                 `{checked}` instead (regenerate via `cpp-pkg \
                                 gen`)"
                            );
                        }
                        None => {
                            let declared: Vec<String> = self
                                .gen_outputs
                                .iter()
                                .filter(|(_, (_, ci))| !ci)
                                .map(|(p, _)| p.display().to_string())
                                .collect();
                            bail!(
                                "target `{tname}`: source `{entry}` matches no \
                                 declared [generate] output (declared: {})",
                                if declared.is_empty() {
                                    "none".to_string()
                                } else {
                                    declared.join(", ")
                                }
                            );
                        }
                    }
                }
                None => {
                    // Interpolate for escape handling + stray-var errors.
                    let s = interp::interpolate(
                        entry,
                        InterpPos::SourceOrIncludeEntry,
                        self.ictx,
                    )
                    .map_err(|e| anyhow!("target `{tname}`: in source `{entry}`: {e}"))?;
                    disk_patterns.push(s);
                }
            }
        }
        let sources = expand_patterns(self.root, &disk_patterns, &mut self.warnings)?;

        // The target's own requirement set: own includes/defines (private
        // then public — both apply when compiling the target itself), then
        // each closure node's contribution in first-reach order. Closure
        // nodes contribute only what they PROPAGATE (a sibling's private
        // includes stay its own).
        let mut has_gen_include = false;
        let mut reqs = Reqs::default();
        for inc in eff.includes.private.iter().chain(eff.includes.public.iter()) {
            let (dir, is_gen) = self.resolve_include_entry(tname, inc)?;
            has_gen_include |= is_gen;
            reqs.includes.push((dir, false));
        }
        for def in eff.defines.private.iter().chain(eff.defines.public.iter()) {
            reqs.defines.push(self.interp_define(tname, def)?);
        }
        // Propagated flags (§1.3 step 5): collected alongside, in the SAME
        // closure order, deduplicated by contributing node (the seen-set of
        // the closure walk), never by flag string.
        let mut prop_cxx: Vec<String> = Vec::new();
        let mut prop_c: Vec<String> = Vec::new();
        for node in closure {
            match node {
                Node::Sibling(s) => {
                    let dep_eff = self.effective[s].clone();
                    let dep_spec = &project.targets[s];
                    // `system-includes = true` on a project target moves its
                    // CONSUMERS' view of its public includes to -isystem;
                    // its own TUs still see -I (§1.1).
                    let as_system = dep_spec.system_includes == Some(true);
                    for inc in &dep_eff.includes.public {
                        let (dir, is_gen) = self.resolve_include_entry(s, inc)?;
                        has_gen_include |= is_gen;
                        // Generated includes are never -isystem (§4.2).
                        reqs.includes.push((dir, as_system && !is_gen));
                    }
                    for def in &dep_eff.defines.public {
                        reqs.defines.push(self.interp_define(s, def)?);
                    }
                    prop_cxx.extend(dep_eff.cxx_flags.public.iter().cloned());
                    prop_c.extend(dep_eff.c_flags.public.iter().cloned());
                }
                Node::Comp { pkg, name } => {
                    let comp = self.component(pkg, name)?;
                    let force_i = self.dep_system_includes_off(pkg);
                    for i in &comp.includes {
                        reqs.includes.push((i.clone(), false));
                    }
                    for i in &comp.system_includes {
                        reqs.includes.push((i.clone(), !force_i));
                    }
                    reqs.defines.extend(comp.defines.iter().cloned());
                    prop_cxx.extend(comp.compile_options.iter().cloned());
                    prop_c.extend(comp.compile_options.iter().cloned());
                    reqs.cxx_std = max_std(reqs.cxx_std, comp.cxx_std);
                }
                Node::BuiltinThreads => {
                    let (compile, _link) = toolchain::threads_expansion(self.truth.os);
                    prop_cxx.extend(compile.iter().map(|s| s.to_string()));
                    prop_c.extend(compile.iter().map(|s| s.to_string()));
                }
            }
        }
        reqs.dedup();
        let effective_cxx = max_std(spec.cxx_std, reqs.cxx_std);

        // §1.3 compile-line stack per language, last-wins:
        // [flags] non-ABI (+cfg) -> profile -> propagated -> own public ->
        // own private. (Layers 1-2 — driver defaults and the ABI injection
        // set — are emitted by ninja/cli, not here.)
        let stack_for = |lang: Lang| -> Vec<String> {
            let mut flags = package_compile_flags(&project.flags, self.truth, lang);
            match lang {
                Lang::Cxx => {
                    flags.extend(self.profile.cxx_flags.iter().cloned());
                    flags.extend(prop_cxx.iter().cloned());
                    flags.extend(eff.cxx_flags.public.iter().cloned());
                    flags.extend(eff.cxx_flags.private.iter().cloned());
                }
                Lang::C => {
                    flags.extend(self.profile.c_flags.iter().cloned());
                    flags.extend(prop_c.iter().cloned());
                    flags.extend(eff.c_flags.public.iter().cloned());
                    flags.extend(eff.c_flags.private.iter().cloned());
                }
            }
            flags
        };
        let flags_cxx = stack_for(Lang::Cxx);
        let flags_c = stack_for(Lang::C);

        let mut units = Vec::new();
        for src in sources.into_iter().chain(gen_sources.iter().cloned()) {
            let is_gen = gen_sources.contains(&src);
            let lang = classify_source(&src)?;
            units.push(CompileUnit {
                lang,
                std: match lang {
                    Lang::Cxx => effective_cxx,
                    Lang::C => spec.c_std,
                },
                includes: reqs.includes.clone(),
                defines: reqs.defines.clone(),
                extra_flags: match lang {
                    Lang::Cxx => flags_cxx.clone(),
                    Lang::C => flags_c.clone(),
                },
                object: object_path(tname, self.root, &src),
                references_gen: is_gen || has_gen_include,
                source: src,
            });
        }

        // Interface sources become units of the CONSUMER, compiled with the
        // providing component's usage requirements (not the consumer's own
        // private ones). One object rule per output — the first-reached
        // component's requirements win.
        let mut seen_objects: BTreeSet<PathBuf> =
            units.iter().map(|u| u.object.clone()).collect();
        for node in closure {
            let Node::Comp { pkg, name } = node else {
                continue;
            };
            let comp = self.component(pkg, name)?;
            if comp.interface_sources.is_empty() {
                continue;
            }
            let mut creqs = self.comp_reqs(pkg, name, &mut Vec::new())?;
            creqs.dedup();
            for src in &comp.interface_sources {
                let lang = classify_source(src)?;
                let object = object_path(tname, self.root, src);
                if !seen_objects.insert(object.clone()) {
                    continue;
                }
                // Interface units carry the component's options over the
                // ambient [flags]/profile base (no consumer target flags:
                // this is the component's code, not the target's).
                let mut extra =
                    package_compile_flags(&project.flags, self.truth, lang);
                match lang {
                    Lang::Cxx => extra.extend(self.profile.cxx_flags.iter().cloned()),
                    Lang::C => extra.extend(self.profile.c_flags.iter().cloned()),
                }
                extra.extend(creqs.options.iter().cloned());
                units.push(CompileUnit {
                    source: src.clone(),
                    lang,
                    std: match lang {
                        Lang::Cxx => max_std(effective_cxx, creqs.cxx_std),
                        Lang::C => spec.c_std,
                    },
                    includes: creqs.includes.clone(),
                    defines: creqs.defines.clone(),
                    extra_flags: extra,
                    object,
                    references_gen: has_gen_include,
                });
            }
        }

        if units.is_empty() {
            let inactive = &eff.inactive_source_groups;
            if inactive.is_empty() {
                bail!("target `{tname}` has no sources (globs matched nothing)");
            }
            bail!(
                "target `{tname}` has no sources (globs matched nothing; cfg \
                 groups [{}] did not match this toolchain)",
                inactive.join(", ")
            );
        }
        Ok(units)
    }

    /// Rule-5 link plan for an executable: own objects, the target's own
    /// link-flag words (§1.3: they follow the objects — raw `-l` words must
    /// come after the objects that reference them), then a pre-order walk
    /// over ALL edges (public, private, link-only) emitting artifacts with
    /// each member's link-flag words glued behind its archive, then the
    /// archive-keep-last / rest-keep-first dedup.
    fn link_plan(
        &mut self,
        tname: &str,
        units: &[CompileUnit],
    ) -> Result<(Vec<LinkInput>, Vec<String>)> {
        let project: &'a ProjectFile = self.project;
        let eff = self.effective[tname].clone();
        let mut raw: Vec<RawLink> = units
            .iter()
            .map(|u| RawLink {
                input: LinkInput::Object(u.object.clone()),
                flag_key: None,
            })
            .collect();
        let own_contrib = format!("target:{tname}");
        for (i, w) in eff
            .link_flags
            .public
            .iter()
            .chain(eff.link_flags.private.iter())
            .enumerate()
        {
            raw.push(RawLink {
                input: LinkInput::Flag(w.clone()),
                flag_key: Some((own_contrib.clone(), i)),
            });
        }
        let edges = self.edges(tname)?;
        // The executable itself sits on the path stack so an edge cycling
        // back to it is reported as a cycle, not infinite recursion.
        let mut stack = vec![Node::Sibling(tname.to_string())];
        for n in edges.public.iter().chain(edges.private.iter()) {
            self.emit_link(n, &mut raw, &mut stack)?;
        }
        let inputs = dedup_link_inputs(raw);
        // §1.3 link line: [flags].link-flags (non-ABI, +cfg) then profile
        // link-flags ride the $link_flags var; own + closure words ride the
        // ordered stream above.
        let mut opts = package_link_flags(&project.flags, self.truth);
        opts.extend(self.profile.link_flags.iter().cloned());
        Ok((inputs, opts))
    }

    fn emit_link(
        &mut self,
        node: &Node,
        out: &mut Vec<RawLink>,
        stack: &mut Vec<Node>,
    ) -> Result<()> {
        if stack.contains(node) {
            let mut cycle: Vec<String> = stack
                .iter()
                .skip_while(|n| *n != node)
                .map(Node::describe)
                .collect();
            cycle.push(node.describe());
            bail!("dependency cycle in link closure: {}", cycle.join(" -> "));
        }
        // Diamond-heavy graphs re-emit shared subtrees (that is what makes
        // keep-last dedup meaningful); cap runaway expansion just in case.
        if out.len() > 100_000 {
            bail!("link closure expansion exceeded 100000 entries; dependency graph too dense");
        }
        stack.push(node.clone());
        let contrib = node.describe();
        let push_flags = |out: &mut Vec<RawLink>, words: &[String]| {
            for (i, w) in words.iter().enumerate() {
                out.push(RawLink {
                    input: LinkInput::Flag(w.clone()),
                    flag_key: Some((contrib.clone(), i)),
                });
            }
        };
        match node {
            Node::Sibling(s) => {
                let project: &'a ProjectFile = self.project;
                let spec = &project.targets[s];
                match spec.kind {
                    TargetKind::StaticLibrary => {
                        out.push(RawLink {
                            input: LinkInput::Archive(output_path(s, spec.kind)),
                            flag_key: None,
                        });
                        // §1.3: a static library's link-flags propagate
                        // link-only to consumers' final links, glued behind
                        // its archive (public ≡ private here until a shared
                        // kind exists).
                        let eff = self.effective[s].clone();
                        let words: Vec<String> = eff
                            .link_flags
                            .public
                            .iter()
                            .chain(eff.link_flags.private.iter())
                            .cloned()
                            .collect();
                        push_flags(out, &words);
                    }
                    TargetKind::Executable => {
                        bail!("target `{s}` is an executable and cannot appear in a link closure")
                    }
                }
                let e = self.edges(s)?;
                // Private deps of a static library propagate as link-only
                // edges: traversed here, invisible to compile propagation.
                for n in e.public.iter().chain(e.private.iter()) {
                    self.emit_link(n, out, stack)?;
                }
            }
            Node::Comp { pkg, name } => {
                let comp = self.component(pkg, name)?;
                let cfg = cmake_config_name(self.config);
                let location = comp.location.get(cfg).cloned();
                let missing = || {
                    anyhow!(
                        "component `{name}` of dependency `{pkg}` has no \
                         artifact for config {cfg}"
                    )
                };
                let mut push = |input: LinkInput| {
                    out.push(RawLink {
                        input,
                        flag_key: None,
                    })
                };
                match comp.kind {
                    Some(ComponentKind::Archive) => {
                        push(LinkInput::Archive(location.ok_or_else(missing)?))
                    }
                    Some(ComponentKind::Dylib) => {
                        push(LinkInput::Dylib(location.ok_or_else(missing)?))
                    }
                    Some(ComponentKind::Interface) => {}
                    Some(ComponentKind::Unknown) | None => {
                        if let Some(loc) = location {
                            push(classify_artifact(loc));
                        }
                    }
                }
                for p in &comp.link_paths {
                    push(classify_artifact(p.clone()));
                }
                for l in &comp.system_libs {
                    push(LinkInput::SystemLib(l.clone()));
                }
                for f in &comp.frameworks {
                    push(LinkInput::Framework(f.clone()));
                }
                // §1.3 interleaving: the member's own link-option words land
                // directly behind its artifacts, never in a trailing block
                // (GNU ld --as-needed would discard `-lrt`-class inputs
                // emitted before the archive that references them).
                push_flags(out, &comp.link_options);
                let context = format!("required by component `{name}` of dependency `{pkg}`");
                let refs: Vec<&'a String> =
                    comp.requires.iter().chain(comp.link_requires.iter()).collect();
                for r in refs {
                    let child = self.resolve(r, &context)?;
                    if matches!(child, Node::Sibling(_)) {
                        bail!(
                            "component `{name}` of dependency `{pkg}` requires \
                             `{r}`, which is a target of this project"
                        );
                    }
                    self.emit_link(&child, out, stack)?;
                }
            }
            Node::BuiltinThreads => {
                // §5.4: pure function of the toolchain os axis; linux gets
                // -pthread at this closure position, darwin/msvc nothing.
                let (_compile, link) = toolchain::threads_expansion(self.truth.os);
                let words: Vec<String> = link.iter().map(|s| s.to_string()).collect();
                push_flags(out, &words);
            }
        }
        stack.pop();
        Ok(())
    }

    /// Deterministic dependencies-first order over the selected targets'
    /// direct sibling edges (Kahn; ties resolve alphabetically).
    fn topo(&mut self, selected: &BTreeSet<String>) -> Result<Vec<String>> {
        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for t in selected {
            let e = self.edges(t)?;
            let mut direct = BTreeSet::new();
            for n in e.public.iter().chain(e.private.iter()) {
                if let Node::Sibling(s) = n
                    && selected.contains(s)
                {
                    direct.insert(s.clone());
                    dependents.entry(s.clone()).or_default().insert(t.clone());
                }
            }
            deps.insert(t.clone(), direct);
        }
        let mut ready: BTreeSet<String> = deps
            .iter()
            .filter(|(_, d)| d.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        let mut out: Vec<String> = Vec::new();
        while let Some(t) = ready.iter().next().cloned() {
            ready.remove(&t);
            out.push(t.clone());
            if let Some(ds) = dependents.get(&t).cloned() {
                for d in ds {
                    let set = deps.get_mut(&d).expect("dependent is selected");
                    set.remove(&t);
                    if set.is_empty() {
                        ready.insert(d);
                    }
                }
            }
        }
        if out.len() != selected.len() {
            let remaining: Vec<String> = selected
                .iter()
                .filter(|t| !out.contains(t))
                .cloned()
                .collect();
            bail!("dependency cycle among targets: {}", remaining.join(" -> "));
        }
        Ok(out)
    }

    // -- generate steps (§4.2) ---------------------------------------------

    /// Activation + ordering: a step is activated when the requested set
    /// references its output through a ${gen} source (exact), a ${gen}
    /// include dir (prefix), or — for test runs — a run-entry value; plus
    /// every step reachable through ${gen} input -> output edges. Activated
    /// steps' inputs are validated; dormant steps are never touched.
    fn plan_gen_steps(
        &mut self,
        selected: &BTreeSet<String>,
        request: &BuildRequest,
    ) -> Result<Vec<PlannedGenStep>> {
        let project: &'a ProjectFile = self.project;
        if project.generate.is_empty() {
            return Ok(Vec::new());
        }

        // Reference collection (raw manifest strings of the selection).
        let mut exact: BTreeSet<PathBuf> = BTreeSet::new();
        let mut prefixes: BTreeSet<PathBuf> = BTreeSet::new();
        for t in selected {
            let eff = &self.effective[t];
            for s in &eff.sources {
                if let Some(rel) = gen_rel_of(s)
                    .map_err(|e| anyhow!("target `{t}`: source {e}"))?
                {
                    exact.insert(PathBuf::from(rel));
                }
            }
            for inc in eff.includes.public.iter().chain(eff.includes.private.iter()) {
                if let Some(rel) = gen_rel_of(inc)
                    .map_err(|e| anyhow!("target `{t}`: include {e}"))?
                {
                    prefixes.insert(PathBuf::from(rel));
                }
            }
            if matches!(request, BuildRequest::Tests(_)) && project.targets[t].test {
                for e in &project.targets[t].run {
                    let values = e
                        .args
                        .iter()
                        .chain(e.cwd.iter())
                        .chain(e.env.values());
                    for v in values {
                        for p in gen_prefixes_in(v) {
                            prefixes.insert(PathBuf::from(p));
                        }
                    }
                }
            }
        }
        if exact.is_empty() && prefixes.is_empty() {
            return Ok(Vec::new());
        }

        // Seed activation from references. checked-in steps live outside
        // the build graph (§4.2) and never activate here.
        let mut activated: BTreeSet<String> = BTreeSet::new();
        for (rel, (step, checked_in)) in &self.gen_outputs {
            if *checked_in {
                continue;
            }
            let hit = exact.contains(rel)
                || prefixes
                    .iter()
                    .any(|p| p.as_os_str().is_empty() || rel.starts_with(p));
            if hit {
                activated.insert(step.clone());
            }
        }

        // Closure over ${gen} input -> output edges; validate inputs of
        // every activated step (missing tree input = plan-time hard error
        // naming the path — §4.2, scoped to the activated set).
        let mut edges_map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new(); // step -> producers
        let mut resolved_inputs: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut work: Vec<String> = activated.iter().cloned().collect();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        while let Some(name) = work.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let step = &project.generate[&name];
            let mut producers: BTreeSet<String> = BTreeSet::new();
            let mut inputs: Vec<PathBuf> = Vec::new();
            let mut declared: Vec<&String> = step.inputs.iter().collect();
            if let GenerateAction::Template { template, .. } = &step.action {
                declared.push(template);
            }
            if let GenerateAction::Command {
                stdin: Some(stdin), ..
            } = &step.action
            {
                declared.push(stdin);
            }
            for raw in declared {
                match gen_rel_of(raw)
                    .map_err(|e| anyhow!("generate step `{name}`: input {e}"))?
                {
                    Some(rel) => {
                        let rel_path = PathBuf::from(&rel);
                        match self.gen_outputs.get(&rel_path) {
                            Some((producer, false)) => {
                                producers.insert(producer.clone());
                                work.push(producer.clone());
                                inputs.push(self.gen_root_abs.join(rel_path));
                            }
                            Some((producer, true)) => bail!(
                                "generate step `{name}`: input `{raw}` names \
                                 the output of checked-in step `{producer}`, \
                                 which is outside the build graph — reference \
                                 its checked-in path instead"
                            ),
                            None => bail!(
                                "generate step `{name}`: input `{raw}` does \
                                 not match any declared [generate] output"
                            ),
                        }
                    }
                    None => {
                        let s = interp::interpolate(
                            raw,
                            InterpPos::GenerateVarOrArgv,
                            self.ictx,
                        )
                        .map_err(|e| anyhow!("generate step `{name}`: in input `{raw}`: {e}"))?;
                        let p = self.root.join(&s);
                        if !p.is_file() {
                            bail!(
                                "generate step `{name}`: declared input `{s}` \
                                 not found (looked at {})",
                                p.display()
                            );
                        }
                        inputs.push(PathBuf::from(s));
                    }
                }
            }
            edges_map.insert(name.clone(), producers);
            resolved_inputs.insert(name.clone(), inputs);
        }
        let activated = visited;

        // Kahn over producer edges (producers first; name ties alphabetical).
        let mut remaining: BTreeMap<String, BTreeSet<String>> = edges_map.clone();
        let mut order: Vec<String> = Vec::new();
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .find(|(_, deps)| deps.iter().all(|d| !remaining.contains_key(d.as_str())))
                .map(|(k, _)| k.clone());
            match next {
                Some(k) => {
                    remaining.remove(&k);
                    order.push(k);
                }
                None => {
                    let cycle: Vec<String> = remaining.keys().cloned().collect();
                    bail!(
                        "cycle among [generate] steps (via ${{gen}} inputs): {}",
                        cycle.join(", ")
                    );
                }
            }
        }

        let mut steps = Vec::new();
        for name in order {
            let step = &project.generate[&name];
            let action = self.interp_action(&name, &step.action)?;
            steps.push(PlannedGenStep {
                output: gen_output_rel(&step.action),
                inputs: resolved_inputs.remove(&name).unwrap_or_default(),
                name,
                action,
            });
        }
        let _ = activated;
        Ok(steps)
    }

    /// Interpolate a step's vars/argv/stdin/template (§0.3 position:
    /// generate vars/argv). Output paths stay literal (schema-validated).
    fn interp_action(&self, step: &str, action: &GenerateAction) -> Result<GenerateAction> {
        let one = |v: &str| -> Result<String> {
            // ${gen} in argv resolves to the ABSOLUTE gen root: gen-exec
            // runs from the project root, but the output tree may be
            // configured elsewhere.
            if let Some(rel) = gen_rel_of(v)
                .map_err(|e| anyhow!("generate step `{step}`: {e}"))?
            {
                let p = if rel.is_empty() {
                    self.gen_root_abs.clone()
                } else {
                    self.gen_root_abs.join(rel)
                };
                return Ok(p.to_string_lossy().into_owned());
            }
            interp::interpolate(v, InterpPos::GenerateVarOrArgv, self.ictx)
                .map_err(|e| anyhow!("generate step `{step}`: in `{v}`: {e}"))
        };
        Ok(match action {
            GenerateAction::Template {
                template,
                output,
                vars,
            } => {
                let mut ivars = BTreeMap::new();
                for (k, v) in vars {
                    ivars.insert(k.clone(), one(v)?);
                }
                GenerateAction::Template {
                    template: one(template)?,
                    output: output.clone(),
                    vars: ivars,
                }
            }
            GenerateAction::Command { argv, stdin, stdout } => {
                let mut iargv = Vec::new();
                for a in argv {
                    iargv.push(one(a)?);
                }
                GenerateAction::Command {
                    argv: iargv,
                    stdin: match stdin {
                        Some(s) => Some(one(s)?),
                        None => None,
                    },
                    stdout: stdout.clone(),
                }
            }
        })
    }

    // -- runtime data (§6.5) -----------------------------------------------

    /// Build-time staging plan for the selection. Destination collisions:
    /// byte-equal sources dedupe (two targets may declare the same data and
    /// share the staged copies); different bytes for one destination is a
    /// hard error.
    fn plan_data_stages(&mut self, selected: &BTreeSet<String>) -> Result<Vec<DataStage>> {
        // dest -> (first src, owning targets)
        let mut stages: BTreeMap<PathBuf, (PathBuf, BTreeSet<String>)> = BTreeMap::new();
        for tname in selected {
            let eff = self.effective[tname].clone();
            for rd in &eff.runtime_data {
                let from_dir = self.root.join(&rd.from);
                if !from_dir.is_dir() {
                    bail!(
                        "target `{tname}`: runtime-data `from = \"{}\"` is not \
                         a directory (looked at {})",
                        rd.from,
                        from_dir.display()
                    );
                }
                let patterns: Vec<String> = if rd.patterns.is_empty() {
                    vec!["**/*".to_string()]
                } else {
                    rd.patterns.clone()
                };
                let files = expand_patterns(&from_dir, &patterns, &mut self.warnings)?;
                for f in files {
                    let rel = f
                        .strip_prefix(&from_dir)
                        .expect("expansion stays under its base");
                    let dest = PathBuf::from(&rd.to).join(rel);
                    match stages.get_mut(&dest) {
                        None => {
                            stages.insert(
                                dest,
                                (f, BTreeSet::from([tname.clone()])),
                            );
                        }
                        Some((first, owners)) => {
                            if *first != f {
                                let a = std::fs::read(&*first).map_err(|e| {
                                    anyhow!("reading {}: {e}", first.display())
                                })?;
                                let b = std::fs::read(&f)
                                    .map_err(|e| anyhow!("reading {}: {e}", f.display()))?;
                                if a != b {
                                    bail!(
                                        "runtime-data destination `{}` receives \
                                         different bytes from `{}` and `{}`",
                                        dest.display(),
                                        first.display(),
                                        f.display()
                                    );
                                }
                            }
                            owners.insert(tname.clone());
                        }
                    }
                }
            }
        }
        Ok(stages
            .into_iter()
            .map(|(dest, (src, owners))| DataStage {
                src,
                dest,
                for_targets: owners.into_iter().collect(),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        DependencySpec, ExposesTargets, GitRef, PackageMeta, Profile, SourceSpec, TargetSpec,
        VisibilitySplit,
    };
    use std::fs;
    use tempfile::TempDir;

    // ---- fixture helpers -------------------------------------------------

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn vis(public: &[&str], private: &[&str]) -> VisibilitySplit {
        VisibilitySplit {
            public: strings(public),
            private: strings(private),
        }
    }

    fn target(kind: TargetKind, sources: &[&str]) -> TargetSpec {
        TargetSpec {
            kind,
            sources: strings(sources),
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

    fn dep() -> DependencySpec {
        DependencySpec {
            source: SourceSpec::Git {
                url: "https://example.invalid/repo".into(),
                reference: GitRef::Tag("v1".into()),
            },
            options: BTreeMap::new(),
            needs: vec![],
            find_package: None,
            exposes_namespace: vec![],
            exposes_targets: ExposesTargets::default(),
            patches: vec![],
            subdir: None,
            cfg: None,
            dev: false,
            system_includes: None,
        }
    }

    fn empty_flags() -> PackageFlags {
        PackageFlags {
            cxx_flags: vec![],
            c_flags: vec![],
            link_flags: vec![],
            cfg: vec![],
        }
    }

    fn project(
        deps: BTreeMap<String, DependencySpec>,
        targets: BTreeMap<String, TargetSpec>,
    ) -> ProjectFile {
        ProjectFile {
            package: PackageMeta {
                name: "proj".into(),
                version: None,
            },
            toolchains: BTreeMap::new(),
            profiles: BTreeMap::new(),
            flags: empty_flags(),
            dependencies: deps,
            dev_dependencies: BTreeMap::new(),
            generate: BTreeMap::new(),
            export: crate::schema::ExportMeta {
                cmake_name: "proj".into(),
                namespace: "proj".into(),
            },
            targets,
            target_defaults_raw: None,
        }
    }

    fn archive(loc: &str) -> Component {
        Component {
            kind: Some(ComponentKind::Archive),
            location: BTreeMap::from([("Release".to_string(), PathBuf::from(loc))]),
            ..Default::default()
        }
    }

    fn manifest(pkg: &str, comps: &[(&str, Component)]) -> Manifest {
        Manifest {
            package: pkg.into(),
            components: comps
                .iter()
                .map(|(n, c)| (n.to_string(), c.clone()))
                .collect(),
            notes: vec![],
        }
    }

    fn macos_truth() -> CfgTruth {
        CfgTruth {
            os: CfgAtom::Macos,
            compiler: CfgAtom::Clang,
        }
    }

    fn linux_truth() -> CfgTruth {
        CfgTruth {
            os: CfgAtom::Linux,
            compiler: CfgAtom::Gcc,
        }
    }

    fn pred(atom: CfgAtom) -> CfgPredicate {
        CfgPredicate { atom }
    }

    /// Full-control plan entry point for tests.
    fn plan_full(
        project: &ProjectFile,
        root: &Path,
        manifests: &BTreeMap<String, Manifest>,
        profile: &Profile,
        truth: &CfgTruth,
        request: &BuildRequest,
    ) -> Result<BuildPlan> {
        let pins = BTreeMap::new();
        let gen_root = root.join("build/gen");
        let build_dir = root.join("build");
        let version = project.package.version.clone();
        let ictx = InterpCtx {
            package_name: &project.package.name,
            package_version: version.as_deref(),
            pins: &pins,
            gen_root: Some(&gen_root),
            project_root: Some(root),
            build_dir: Some(&build_dir),
            install_prefix: None,
        };
        plan(&PlanInputs {
            project,
            project_root: root,
            manifests,
            config: BuildConfig::Release,
            profile,
            cfg_truth: truth,
            request: &request.clone(),
            interp: &ictx,
        })
    }

    fn run_plan(
        project: &ProjectFile,
        root: &Path,
        manifests: &BTreeMap<String, Manifest>,
    ) -> Result<BuildPlan> {
        plan_full(
            project,
            root,
            manifests,
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Default,
        )
    }

    fn find<'p>(plan: &'p BuildPlan, name: &str) -> &'p PlannedTarget {
        plan.targets.iter().find(|t| t.name == name).unwrap()
    }

    fn archive_paths(t: &PlannedTarget) -> Vec<String> {
        t.link_inputs
            .iter()
            .filter_map(|li| match li {
                LinkInput::Archive(p) => Some(p.display().to_string()),
                _ => None,
            })
            .collect()
    }

    fn include_dirs(u: &CompileUnit) -> Vec<String> {
        u.includes.iter().map(|(p, _)| p.display().to_string()).collect()
    }

    /// One-executable project depending on the given references.
    fn exe_project(deps: BTreeMap<String, DependencySpec>, refs: &[&str]) -> (ProjectFile, TempDir) {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src/main.cpp");
        let mut t = target(TargetKind::Executable, &["src/main.cpp"]);
        t.dependencies = vis(refs, &[]);
        let p = project(deps, BTreeMap::from([("app".to_string(), t)]));
        (p, tmp)
    }

    // ---- naming ladder ---------------------------------------------------

    #[test]
    fn ladder_step1_unique_name_resolves_directly() {
        let (p, tmp) = exe_project(BTreeMap::from([("fmt".to_string(), dep())]), &["fmt::fmt"]);
        let manifests =
            BTreeMap::from([("fmt".to_string(), manifest("fmt", &[("fmt::fmt", archive("/store/fmt/libfmt.a"))]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(archive_paths(find(&plan, "app")), vec!["/store/fmt/libfmt.a"]);
    }

    #[test]
    fn ladder_step2_depkey_prefix_owns() {
        let deps = BTreeMap::from([("fmt".to_string(), dep()), ("spdlog".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["fmt::fmt"]);
        let manifests = BTreeMap::from([
            ("fmt".to_string(), manifest("fmt", &[("fmt::fmt", archive("/store/fmt/libfmt.a"))])),
            (
                "spdlog".to_string(),
                manifest(
                    "spdlog",
                    &[
                        ("fmt::fmt", archive("/store/spdlog/libfmt-shadow.a")),
                        ("spdlog::spdlog", archive("/store/spdlog/libspdlog.a")),
                    ],
                ),
            ),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(archive_paths(find(&plan, "app")), vec!["/store/fmt/libfmt.a"]);
    }

    #[test]
    fn ladder_step3_exposes_namespace_claims() {
        let mut fmtlib = dep();
        fmtlib.exposes_namespace = vec!["fmt".to_string()];
        let deps = BTreeMap::from([("fmtlib".to_string(), fmtlib), ("spd".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["fmt::fmt"]);
        let manifests = BTreeMap::from([
            ("fmtlib".to_string(), manifest("fmtlib", &[("fmt::fmt", archive("/store/fmtlib/libfmt.a"))])),
            (
                "spd".to_string(),
                manifest(
                    "spd",
                    &[
                        ("fmt::fmt", archive("/store/spd/libfmt-shadow.a")),
                        ("spd::spd", archive("/store/spd/libspd.a")),
                    ],
                ),
            ),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(archive_paths(find(&plan, "app")), vec!["/store/fmtlib/libfmt.a"]);
    }

    #[test]
    fn ladder_step3_exposes_targets_list_claims() {
        let mut fmtlib = dep();
        fmtlib.exposes_targets.claims = vec!["fmt::fmt".to_string()];
        let deps = BTreeMap::from([("fmtlib".to_string(), fmtlib), ("spd".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["fmt::fmt"]);
        let manifests = BTreeMap::from([
            ("fmtlib".to_string(), manifest("fmtlib", &[("fmt::fmt", archive("/store/fmtlib/libfmt.a"))])),
            ("spd".to_string(), manifest("spd", &[("fmt::fmt", archive("/store/spd/libfmt-shadow.a"))])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(archive_paths(find(&plan, "app")), vec!["/store/fmtlib/libfmt.a"]);
    }

    #[test]
    fn ladder_step3_exposes_targets_map_renames() {
        let mut a = dep();
        a.exposes_targets.renames =
            BTreeMap::from([("zoo::zoo".to_string(), "zoo-a".to_string())]);
        let deps = BTreeMap::from([("a".to_string(), a), ("b".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["zoo-a", "zoo::zoo"]);
        let manifests = BTreeMap::from([
            ("a".to_string(), manifest("a", &[("zoo::zoo", archive("/store/a/libzoo.a"))])),
            ("b".to_string(), manifest("b", &[("zoo::zoo", archive("/store/b/libzoo.a"))])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let archives = archive_paths(find(&plan, "app"));
        assert_eq!(archives, vec!["/store/a/libzoo.a", "/store/b/libzoo.a"]);
    }

    #[test]
    fn ladder_step4_ambiguity_is_hard_error_listing_candidates() {
        let deps = BTreeMap::from([("alpha".to_string(), dep()), ("beta".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["boost::core"]);
        let manifests = BTreeMap::from([
            ("alpha".to_string(), manifest("alpha", &[("boost::core", archive("/a.a"))])),
            ("beta".to_string(), manifest("beta", &[("boost::core", archive("/b.a"))])),
        ]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("boost::core"), "{err}");
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");
        assert!(err.contains("exposes-namespace"), "{err}");
        assert!(err.contains("exposes-targets"), "{err}");
    }

    #[test]
    fn ladder_step2_prefix_decisive_over_exposes_claims() {
        let mut a = dep();
        a.exposes_namespace = vec!["foo".to_string()];
        let deps = BTreeMap::from([
            ("foo".to_string(), dep()),
            ("a".to_string(), a),
            ("b".to_string(), dep()),
        ]);
        let (p, tmp) = exe_project(deps, &["foo::bar"]);
        let manifests = BTreeMap::from([
            ("foo".to_string(), manifest("foo", &[("foo::other", archive("/foo.a"))])),
            ("a".to_string(), manifest("a", &[("foo::bar", archive("/a.a"))])),
            ("b".to_string(), manifest("b", &[("foo::bar", archive("/b.a"))])),
        ]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("does not export"), "{err}");
        assert!(err.contains("foo::bar"), "{err}");
        assert!(err.contains("foo::other"), "{err}");
    }

    #[test]
    fn rename_colliding_with_project_target_is_error() {
        let mut fmtdep = dep();
        fmtdep.exposes_targets.renames =
            BTreeMap::from([("fmt::fmt".to_string(), "app".to_string())]);
        let deps = BTreeMap::from([("fmt".to_string(), fmtdep)]);
        let (p, tmp) = exe_project(deps, &[]);
        let manifests = BTreeMap::from([(
            "fmt".to_string(),
            manifest("fmt", &[("fmt::fmt", archive("/f.a"))]),
        )]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("already a target of this project"), "{err}");
        assert!(err.contains("fmt::fmt") && err.contains("app"), "{err}");
    }

    #[test]
    fn shared_interface_source_compiles_once() {
        let mut c1 = Component {
            kind: Some(ComponentKind::Interface),
            ..Default::default()
        };
        c1.interface_sources = vec![PathBuf::from("/store/j/src/stub.cpp")];
        let mut c2 = c1.clone();
        c2.includes = vec![PathBuf::from("/store/j/include")];
        let deps = BTreeMap::from([("j".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["j::a", "j::b"]);
        let manifests = BTreeMap::from([(
            "j".to_string(),
            manifest("j", &[("j::a", c1), ("j::b", c2)]),
        )]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        let stub_units: Vec<_> = app
            .units
            .iter()
            .filter(|u| u.source == *"/store/j/src/stub.cpp")
            .collect();
        assert_eq!(stub_units.len(), 1, "shared interface source must compile once");
        let mut objs: Vec<_> = app.units.iter().map(|u| u.object.clone()).collect();
        objs.sort();
        objs.dedup();
        assert_eq!(objs.len(), app.units.len());
    }

    #[test]
    fn object_path_with_parent_segments_uses_hashed_branch() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(tmp.path().join("x.cpp"), "").unwrap();
        let t = target(TargetKind::Executable, &["src/../../x.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, &root, &BTreeMap::new()).unwrap();
        let obj = find(&plan, "app").units[0].object.display().to_string();
        assert!(obj.starts_with("app.dir/ext/"), "{obj}");
        assert!(obj.ends_with("-x.cpp.o"), "{obj}");
        assert!(!obj.contains(".."), "{obj}");
    }

    #[test]
    fn conflicting_exposes_claims_error() {
        let mut a = dep();
        a.exposes_namespace = vec!["boost".to_string()];
        let mut b = dep();
        b.exposes_namespace = vec!["boost".to_string()];
        let deps = BTreeMap::from([("alpha".to_string(), a), ("beta".to_string(), b)]);
        let (p, tmp) = exe_project(deps, &["boost::core"]);
        let manifests = BTreeMap::from([
            ("alpha".to_string(), manifest("alpha", &[("boost::core", archive("/a.a"))])),
            ("beta".to_string(), manifest("beta", &[("boost::core", archive("/b.a"))])),
        ]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("conflicting"), "{err}");
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");
    }

    #[test]
    fn unknown_reference_error() {
        let (p, tmp) = exe_project(BTreeMap::new(), &["nope::x"]);
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("unknown dependency reference"), "{err}");
        assert!(err.contains("nope::x"), "{err}");
        assert!(err.contains("app"), "{err}");
    }

    #[test]
    fn prefix_matches_dep_but_target_missing_error() {
        let deps = BTreeMap::from([("fmt".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["fmt::missing"]);
        let manifests =
            BTreeMap::from([("fmt".to_string(), manifest("fmt", &[("fmt::fmt", archive("/f.a"))]))]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("does not export"), "{err}");
        assert!(err.contains("fmt::missing"), "{err}");
        assert!(err.contains("fmt::fmt"), "{err}");
    }

    #[test]
    fn executable_as_dependency_error() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src/main.cpp");
        touch(tmp.path(), "src/tool.cpp");
        let mut app = target(TargetKind::Executable, &["src/main.cpp"]);
        app.dependencies = vis(&[], &["tool"]);
        let tool = target(TargetKind::Executable, &["src/tool.cpp"]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("app".to_string(), app), ("tool".to_string(), tool)]),
        );
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("executable"), "{err}");
    }

    // ---- visibility propagation + link-only rule -------------------------

    fn visibility_fixture() -> (ProjectFile, TempDir, BTreeMap<String, Manifest>) {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src/main.cpp");
        touch(tmp.path(), "src/core.cpp");
        let mut core = target(TargetKind::StaticLibrary, &["src/core.cpp"]);
        core.includes = vis(&["include"], &["src"]);
        core.defines = vis(&["CORE_API="], &["CORE_INTERNAL"]);
        core.dependencies = vis(&["fmt::fmt"], &["spdlog::spdlog"]);
        let mut myapp = target(TargetKind::Executable, &["src/main.cpp"]);
        myapp.dependencies = vis(&[], &["core"]);
        let p = project(
            BTreeMap::from([("fmt".to_string(), dep()), ("spdlog".to_string(), dep())]),
            BTreeMap::from([("core".to_string(), core), ("myapp".to_string(), myapp)]),
        );
        let mut fmt_c = archive("/store/fmt/libfmt.a");
        fmt_c.includes = vec![PathBuf::from("/store/fmt/include")];
        let mut spd_c = archive("/store/spdlog/libspdlog.a");
        spd_c.includes = vec![PathBuf::from("/store/spdlog/include")];
        spd_c.defines = vec![("SPDLOG_COMPILED_LIB".to_string(), None)];
        let manifests = BTreeMap::from([
            ("fmt".to_string(), manifest("fmt", &[("fmt::fmt", fmt_c)])),
            ("spdlog".to_string(), manifest("spdlog", &[("spdlog::spdlog", spd_c)])),
        ]);
        (p, tmp, manifests)
    }

    #[test]
    fn public_deps_propagate_private_do_not() {
        let (p, tmp, manifests) = visibility_fixture();
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let root = tmp.path();

        let myapp = find(&plan, "myapp");
        let unit = &myapp.units[0];
        let incs = include_dirs(unit);
        assert!(incs.contains(&root.join("include").display().to_string()), "{incs:?}");
        assert!(incs.contains(&"/store/fmt/include".to_string()), "{incs:?}");
        assert!(!incs.contains(&root.join("src").display().to_string()), "{incs:?}");
        assert!(!incs.contains(&"/store/spdlog/include".to_string()), "{incs:?}");
        assert!(unit.defines.contains(&("CORE_API".to_string(), Some(String::new()))));
        assert!(!unit.defines.iter().any(|(k, _)| k == "CORE_INTERNAL"));
        assert!(!unit.defines.iter().any(|(k, _)| k == "SPDLOG_COMPILED_LIB"));

        let core = find(&plan, "core");
        let cincs = include_dirs(&core.units[0]);
        assert!(cincs.contains(&root.join("src").display().to_string()));
        assert!(cincs.contains(&"/store/spdlog/include".to_string()));
        assert!(core.units[0].defines.iter().any(|(k, _)| k == "CORE_INTERNAL"));
    }

    #[test]
    fn static_library_private_dep_is_link_only_for_consumers() {
        let (p, tmp, manifests) = visibility_fixture();
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let myapp = find(&plan, "myapp");
        let archives = archive_paths(myapp);
        assert!(archives.contains(&"/store/spdlog/libspdlog.a".to_string()), "{archives:?}");
        assert!(archives.contains(&"/store/fmt/libfmt.a".to_string()));
        assert!(archives.contains(&"libcore.a".to_string()));
        let core_pos = archives.iter().position(|a| a == "libcore.a").unwrap();
        let spd_pos = archives.iter().position(|a| a.contains("spdlog")).unwrap();
        assert!(core_pos < spd_pos);
    }

    // ---- interface sources ------------------------------------------------

    #[test]
    fn interface_sources_compile_in_consumer_with_component_reqs() {
        let mut json = Component {
            kind: Some(ComponentKind::Interface),
            ..Default::default()
        };
        json.includes = vec![PathBuf::from("/store/json/include")];
        json.defines = vec![("JSON_DIAG".to_string(), Some("1".to_string()))];
        json.cxx_std = Some(17);
        json.interface_sources = vec![PathBuf::from("/store/json/src/extra.cpp")];
        let deps = BTreeMap::from([("json".to_string(), dep())]);
        let (mut p, tmp) = exe_project(deps, &["json::json"]);
        p.targets.get_mut("app").unwrap().includes = vis(&[], &["appsrc"]);
        let manifests = BTreeMap::from([("json".to_string(), manifest("json", &[("json::json", json)]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        assert_eq!(app.units.len(), 2);
        let unit = app
            .units
            .iter()
            .find(|u| u.source == *"/store/json/src/extra.cpp")
            .expect("interface source became a unit of the CONSUMER");
        assert_eq!(unit.lang, Lang::Cxx);
        assert_eq!(unit.std, Some(17));
        assert_eq!(include_dirs(unit), vec!["/store/json/include"]);
        assert!(unit.defines.contains(&("JSON_DIAG".to_string(), Some("1".to_string()))));
        let obj = unit.object.display().to_string();
        assert!(obj.starts_with("app.dir/ext/"), "{obj}");
        assert!(obj.ends_with("-extra.cpp.o"), "{obj}");
        assert!(archive_paths(app).is_empty());
    }

    // ---- sources: globs + extension table ---------------------------------

    #[test]
    fn glob_expansion_sorted_byte_order() {
        let tmp = TempDir::new().unwrap();
        for f in ["src/b.cpp", "src/a.cpp", "src/sub/c.cpp", "src/z.txt"] {
            touch(tmp.path(), f);
        }
        let mut t = target(TargetKind::Executable, &["src/**/*.cpp"]);
        t.sources.push("src/a.cpp".to_string()); // duplicate -> deduped
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let sources: Vec<String> = find(&plan, "app")
            .units
            .iter()
            .map(|u| u.source.strip_prefix(tmp.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(sources, vec!["src/a.cpp", "src/b.cpp", "src/sub/c.cpp"]);
    }

    #[test]
    fn glob_matching_nothing_yields_no_sources_error() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        let t = target(TargetKind::Executable, &["src/*.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("no sources"), "{err}");
    }

    #[test]
    fn literal_missing_source_error() {
        let tmp = TempDir::new().unwrap();
        let t = target(TargetKind::Executable, &["src/main.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("src/main.cpp"), "{err}");
    }

    #[test]
    fn extension_table_and_errors() {
        let tmp = TempDir::new().unwrap();
        for f in ["a.cpp", "b.cc", "c.cxx", "d.c++", "e.c"] {
            touch(tmp.path(), f);
        }
        let t = target(TargetKind::Executable, &["a.cpp", "b.cc", "c.cxx", "d.c++", "e.c"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let langs: Vec<Lang> = find(&plan, "app").units.iter().map(|u| u.lang).collect();
        assert_eq!(langs, vec![Lang::Cxx, Lang::Cxx, Lang::Cxx, Lang::Cxx, Lang::C]);

        let err = {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), "upper.C");
            let t = target(TargetKind::Executable, &["upper.C"]);
            let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
            run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string()
        };
        assert!(err.contains(".C"), "{err}");
        assert!(err.contains("rename"), "{err}");

        for objc in ["x.m", "y.mm"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), objc);
            let t = target(TargetKind::Executable, &[objc]);
            let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
            let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
            assert!(err.contains("Objective-C not supported in v0"), "{err}");
        }

        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.rs");
        let t = target(TargetKind::Executable, &["main.rs"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("unknown extension"), "{err}");
        assert!(err.contains(".rs"), "{err}");
    }

    // ---- glob negation (§0.4 / §7.1) --------------------------------------

    #[test]
    fn negative_patterns_subtract_from_union() {
        let tmp = TempDir::new().unwrap();
        for f in ["cli/a.cpp", "cli/main.cpp", "cli/z.cpp"] {
            touch(tmp.path(), f);
        }
        let t = target(TargetKind::StaticLibrary, &["cli/*.cpp", "!cli/main.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("cli-lib".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let sources: Vec<String> = find(&plan, "cli-lib")
            .units
            .iter()
            .map(|u| u.source.strip_prefix(tmp.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(sources, vec!["cli/a.cpp", "cli/z.cpp"]);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn negative_pattern_matching_nothing_warns() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src/a.cpp");
        let t = target(TargetKind::Executable, &["src/*.cpp", "!src/gone.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").units.len(), 1);
        assert!(
            plan.warnings.iter().any(|w| w.contains("!src/gone.cpp")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn all_negative_pattern_list_is_error() {
        let mut w = Vec::new();
        let err = expand_patterns(Path::new("/tmp"), &strings(&["!a.cpp"]), &mut w)
            .unwrap_err()
            .to_string();
        assert!(err.contains("only negative"), "{err}");
    }

    // ---- link plan ordering -----------------------------------------------

    #[test]
    fn link_order_topological_archives_keep_last() {
        let tmp = TempDir::new().unwrap();
        for f in ["main.cpp", "a.cpp", "b.cpp", "d.cpp"] {
            touch(tmp.path(), f);
        }
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&[], &["a", "b"]);
        let mut a = target(TargetKind::StaticLibrary, &["a.cpp"]);
        a.dependencies = vis(&["d"], &[]);
        let mut b = target(TargetKind::StaticLibrary, &["b.cpp"]);
        b.dependencies = vis(&["d"], &[]);
        let d = target(TargetKind::StaticLibrary, &["d.cpp"]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([
                ("app".to_string(), app),
                ("a".to_string(), a),
                ("b".to_string(), b),
                ("d".to_string(), d),
            ]),
        );
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let app = find(&plan, "app");
        assert!(matches!(&app.link_inputs[0], LinkInput::Object(_)));
        assert_eq!(archive_paths(app), vec!["liba.a", "libb.a", "libd.a"]);
        let order: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        let pos = |n: &str| order.iter().position(|x| *x == n).unwrap();
        assert!(pos("d") < pos("a") && pos("d") < pos("b") && pos("a") < pos("app"));
    }

    #[test]
    fn system_libs_and_frameworks_dedup_keep_first() {
        let mut x = archive("/store/x/libx.a");
        x.system_libs = vec!["z".to_string()];
        x.frameworks = vec!["CoreFoundation".to_string()];
        let mut y = archive("/store/y/liby.a");
        y.system_libs = vec!["z".to_string(), "m".to_string()];
        y.frameworks = vec!["CoreFoundation".to_string()];
        let deps = BTreeMap::from([("x".to_string(), dep()), ("y".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["x::x", "y::y"]);
        let manifests = BTreeMap::from([
            ("x".to_string(), manifest("x", &[("x::x", x)])),
            ("y".to_string(), manifest("y", &[("y::y", y)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        let sys: Vec<&str> = app
            .link_inputs
            .iter()
            .filter_map(|li| match li {
                LinkInput::SystemLib(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(sys, vec!["z", "m"]);
        let fw: Vec<&str> = app
            .link_inputs
            .iter()
            .filter_map(|li| match li {
                LinkInput::Framework(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(fw, vec!["CoreFoundation"]);
        let z_pos = app
            .link_inputs
            .iter()
            .position(|li| matches!(li, LinkInput::SystemLib(n) if n == "z"))
            .unwrap();
        let y_pos = app
            .link_inputs
            .iter()
            .position(|li| matches!(li, LinkInput::Archive(p) if p.ends_with("liby.a")))
            .unwrap();
        assert!(z_pos < y_pos);
    }

    #[test]
    fn component_cycle_is_error() {
        let mut a = archive("/store/p/liba.a");
        a.requires = vec!["p::b".to_string()];
        let mut b = archive("/store/p/libb.a");
        b.requires = vec!["p::a".to_string()];
        let deps = BTreeMap::from([("p".to_string(), dep())]);
        let (proj, tmp) = exe_project(deps, &["p::a"]);
        let manifests =
            BTreeMap::from([("p".to_string(), manifest("p", &[("p::a", a), ("p::b", b)]))]);
        let err = run_plan(&proj, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("cycle"), "{err}");
        assert!(err.contains("p::a") && err.contains("p::b"), "{err}");
    }

    #[test]
    fn sibling_target_cycle_is_error() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.cpp");
        touch(tmp.path(), "b.cpp");
        let mut a = target(TargetKind::StaticLibrary, &["a.cpp"]);
        a.dependencies = vis(&["b"], &[]);
        let mut b = target(TargetKind::StaticLibrary, &["b.cpp"]);
        b.dependencies = vis(&["a"], &[]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("a".to_string(), a), ("b".to_string(), b)]),
        );
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("cycle"), "{err}");
    }

    // ---- link interleaving (§1.3) ------------------------------------------

    #[test]
    fn member_link_flags_follow_member_archive() {
        // core (project static lib, link-flags -Wl,-x) -> rt::rt (component
        // with link_options -lrt). Both flag words must land IMMEDIATELY
        // after their contributor's archive, never in a trailing block.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "core.cpp");
        let mut rt_c = archive("/store/rt/librt_shim.a");
        rt_c.link_options = vec!["-lrt".to_string()];
        let mut core = target(TargetKind::StaticLibrary, &["core.cpp"]);
        core.dependencies = vis(&["rt::rt"], &[]);
        core.link_flags = vis(&[], &["-Wl,-x"]);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&[], &["core"]);
        let p = project(
            BTreeMap::from([("rt".to_string(), dep())]),
            BTreeMap::from([("app".to_string(), app), ("core".to_string(), core)]),
        );
        let manifests =
            BTreeMap::from([("rt".to_string(), manifest("rt", &[("rt::rt", rt_c)]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        let stream: Vec<String> = app
            .link_inputs
            .iter()
            .map(|li| match li {
                LinkInput::Object(p) => format!("obj:{}", p.display()),
                LinkInput::Archive(p) => format!("ar:{}", p.display()),
                LinkInput::Dylib(p) => format!("dy:{}", p.display()),
                LinkInput::SystemLib(n) => format!("-l{n}"),
                LinkInput::Framework(n) => format!("fw:{n}"),
                LinkInput::Flag(w) => w.clone(),
            })
            .collect();
        let core_pos = stream.iter().position(|s| s == "ar:libcore.a").unwrap();
        assert_eq!(stream[core_pos + 1], "-Wl,-x", "{stream:?}");
        let rt_pos = stream
            .iter()
            .position(|s| s == "ar:/store/rt/librt_shim.a")
            .unwrap();
        assert_eq!(stream[rt_pos + 1], "-lrt", "{stream:?}");
        assert!(core_pos < rt_pos, "dependents before dependencies");
    }

    #[test]
    fn diamond_keeps_member_flags_with_kept_archive() {
        // app -> a, b; both -> d (component with -lrt). d's archive dedups
        // keep-LAST; its flag word must ride along to the kept position.
        let mut d_c = archive("/store/d/libd.a");
        d_c.link_options = vec!["-lrt".to_string()];
        let mut a_c = archive("/store/a/liba.a");
        a_c.requires = vec!["d::d".to_string()];
        let mut b_c = archive("/store/b/libb.a");
        b_c.requires = vec!["d::d".to_string()];
        let deps = BTreeMap::from([
            ("a".to_string(), dep()),
            ("b".to_string(), dep()),
            ("d".to_string(), dep()),
        ]);
        let (p, tmp) = exe_project(deps, &["a::a", "b::b"]);
        let manifests = BTreeMap::from([
            ("a".to_string(), manifest("a", &[("a::a", a_c)])),
            ("b".to_string(), manifest("b", &[("b::b", b_c)])),
            ("d".to_string(), manifest("d", &[("d::d", d_c)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        let flags: Vec<&str> = app
            .link_inputs
            .iter()
            .filter_map(|li| match li {
                LinkInput::Flag(w) => Some(w.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(flags, vec!["-lrt"], "flag contributed once: {flags:?}");
        let d_pos = app
            .link_inputs
            .iter()
            .position(|li| matches!(li, LinkInput::Archive(p) if p.ends_with("libd.a")))
            .unwrap();
        assert!(
            matches!(&app.link_inputs[d_pos + 1], LinkInput::Flag(w) if w == "-lrt"),
            "flag glued to the KEPT (last) archive"
        );
    }

    // ---- link language ----------------------------------------------------

    #[test]
    fn link_language_rules() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.c");
        let t = target(TargetKind::Executable, &["main.c"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::C);

        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let t = target(TargetKind::Executable, &["main.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::Cxx);

        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.c");
        touch(tmp.path(), "lib.cpp");
        let mut app = target(TargetKind::Executable, &["main.c"]);
        app.dependencies = vis(&[], &["lib"]);
        let lib = target(TargetKind::StaticLibrary, &["lib.cpp"]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("app".to_string(), app), ("lib".to_string(), lib)]),
        );
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::Cxx);

        let deps = BTreeMap::from([("z".to_string(), dep())]);
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.c");
        let mut app = target(TargetKind::Executable, &["main.c"]);
        app.dependencies = vis(&["z::z"], &[]);
        let p = project(deps, BTreeMap::from([("app".to_string(), app)]));
        let manifests = BTreeMap::from([("z".to_string(), manifest("z", &[("z::z", archive("/z.a"))]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::Cxx);
    }

    // ---- cxx-std max merge ------------------------------------------------

    #[test]
    fn cxx_std_max_merge_with_components() {
        let mut c17 = archive("/c17.a");
        c17.cxx_std = Some(17);
        let mut c20 = archive("/c20.a");
        c20.cxx_std = Some(20);

        let deps = BTreeMap::from([("d".to_string(), dep())]);
        let (mut p, tmp) = exe_project(deps.clone(), &["d::d"]);
        p.targets.get_mut("app").unwrap().cxx_std = Some(17);
        let manifests = BTreeMap::from([("d".to_string(), manifest("d", &[("d::d", c20.clone())]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(find(&plan, "app").units[0].std, Some(20));

        let (mut p, tmp) = exe_project(deps, &["d::d"]);
        p.targets.get_mut("app").unwrap().cxx_std = Some(23);
        let manifests = BTreeMap::from([("d".to_string(), manifest("d", &[("d::d", c17)]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(find(&plan, "app").units[0].std, Some(23));
    }

    // ---- object path uniquing ----------------------------------------------

    #[test]
    fn object_paths_unique_per_target_for_shared_source() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src/common.cpp");
        let a = target(TargetKind::StaticLibrary, &["src/common.cpp"]);
        let b = target(TargetKind::StaticLibrary, &["src/common.cpp"]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("a".to_string(), a), ("b".to_string(), b)]),
        );
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(
            find(&plan, "a").units[0].object,
            PathBuf::from("a.dir/src/common.cpp.o")
        );
        assert_eq!(
            find(&plan, "b").units[0].object,
            PathBuf::from("b.dir/src/common.cpp.o")
        );
    }

    // ---- request selection --------------------------------------------------

    #[test]
    fn named_request_selects_transitive_sibling_closure() {
        let tmp = TempDir::new().unwrap();
        for f in ["m1.cpp", "m2.cpp", "c1.cpp", "c2.cpp", "u.cpp"] {
            touch(tmp.path(), f);
        }
        let mut app1 = target(TargetKind::Executable, &["m1.cpp"]);
        app1.dependencies = vis(&[], &["core1"]);
        let mut app2 = target(TargetKind::Executable, &["m2.cpp"]);
        app2.dependencies = vis(&[], &["core2"]);
        let mut core1 = target(TargetKind::StaticLibrary, &["c1.cpp"]);
        core1.dependencies = vis(&[], &["util"]); // PRIVATE sibling still selected
        let core2 = target(TargetKind::StaticLibrary, &["c2.cpp"]);
        let util = target(TargetKind::StaticLibrary, &["u.cpp"]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([
                ("app1".to_string(), app1),
                ("app2".to_string(), app2),
                ("core1".to_string(), core1),
                ("core2".to_string(), core2),
                ("util".to_string(), util),
            ]),
        );
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Named(vec!["app1".to_string()]),
        )
        .unwrap();
        let names: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["util", "core1", "app1"]);

        let err = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Named(vec!["nosuch".to_string()]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown target `nosuch`"), "{err}");
    }

    // ---- dev/test markers (§3.2) -------------------------------------------

    /// app (non-dev) + tests (test) + bench (dev) + testing lib (dev).
    fn dev_fixture() -> (ProjectFile, TempDir) {
        let tmp = TempDir::new().unwrap();
        for f in ["main.cpp", "test.cpp", "bench.cpp", "tlib.cpp"] {
            touch(tmp.path(), f);
        }
        let app = target(TargetKind::Executable, &["main.cpp"]);
        let mut tests = target(TargetKind::Executable, &["test.cpp"]);
        tests.test = true;
        tests.dev = true;
        tests.dependencies = vis(&[], &["tlib"]);
        let mut bench = target(TargetKind::Executable, &["bench.cpp"]);
        bench.dev = true;
        let mut tlib = target(TargetKind::StaticLibrary, &["tlib.cpp"]);
        tlib.dev = true;
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([
                ("app".to_string(), app),
                ("tests".to_string(), tests),
                ("bench".to_string(), bench),
                ("tlib".to_string(), tlib),
            ]),
        );
        (p, tmp)
    }

    #[test]
    fn default_build_excludes_dev_targets() {
        let (p, tmp) = dev_fixture();
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let names: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["app"]);
    }

    #[test]
    fn dev_target_buildable_by_explicit_name() {
        let (p, tmp) = dev_fixture();
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Named(vec!["bench".to_string()]),
        )
        .unwrap();
        let names: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["bench"]);
        assert!(find(&plan, "bench").dev);
    }

    #[test]
    fn tests_request_selects_test_targets_and_their_dev_deps() {
        let (p, tmp) = dev_fixture();
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Tests(vec![]),
        )
        .unwrap();
        let names: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["tlib", "tests"]);
        assert!(find(&plan, "tests").test);
    }

    #[test]
    fn tests_filter_matching_nothing_is_error() {
        let (p, tmp) = dev_fixture();
        let err = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Tests(vec!["nomatch".to_string()]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("filter matches no tests"), "{err}");
        assert!(err.contains("tests"), "{err}");
    }

    #[test]
    fn non_dev_target_depending_on_dev_sibling_is_error() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "tlib.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&[], &["tlib"]);
        let mut tlib = target(TargetKind::StaticLibrary, &["tlib.cpp"]);
        tlib.dev = true;
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("app".to_string(), app), ("tlib".to_string(), tlib)]),
        );
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("not a dev target"), "{err}");
        assert!(err.contains("dev target 'tlib'"), "{err}");
    }

    #[test]
    fn non_dev_target_depending_on_dev_dep_component_is_error() {
        // The spec §3.3 case verbatim: a non-dev target referencing a
        // component exported by a dev-dependency.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["GTest::gtest_main"], &[]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        let mut gtest = dep();
        gtest.dev = true;
        p.dev_dependencies.insert("googletest".to_string(), gtest);
        let manifests = BTreeMap::from([(
            "googletest".to_string(),
            manifest("googletest", &[("GTest::gtest_main", archive("/store/gt/libgtest_main.a"))]),
        )]);
        let err = run_plan(&p, tmp.path(), &manifests).unwrap_err().to_string();
        assert!(err.contains("(not a dev target)"), "{err}");
        assert!(err.contains("GTest::gtest_main"), "{err}");
        assert!(err.contains("dev-dependency 'googletest'"), "{err}");
        assert!(err.contains("move 'googletest' to [dependencies]"), "{err}");
    }

    #[test]
    fn dev_test_target_may_use_dev_dep() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "test.cpp");
        let mut tests = target(TargetKind::Executable, &["test.cpp"]);
        tests.test = true;
        tests.dev = true;
        tests.dependencies = vis(&["GTest::gtest_main"], &[]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("tests".to_string(), tests)]));
        let mut gtest = dep();
        gtest.dev = true;
        p.dev_dependencies.insert("googletest".to_string(), gtest);
        let manifests = BTreeMap::from([(
            "googletest".to_string(),
            manifest("googletest", &[("GTest::gtest_main", archive("/store/gt/libgtest_main.a"))]),
        )]);
        let plan = plan_full(
            &p,
            tmp.path(),
            &manifests,
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Tests(vec![]),
        )
        .unwrap();
        assert_eq!(
            archive_paths(find(&plan, "tests")),
            vec!["/store/gt/libgtest_main.a"]
        );
    }

    #[test]
    fn unprovisioned_dev_dep_reference_names_the_cause() {
        // Default build: googletest not provisioned; a (wrongly) non-dev
        // target referencing an attributable name gets the dev explanation.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["GTest::gtest_main"], &[]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        let mut gtest = dep();
        gtest.dev = true;
        gtest.find_package = Some("GTest".to_string());
        p.dev_dependencies.insert("googletest".to_string(), gtest);
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("dev-dependency"), "{err}");
        assert!(err.contains("googletest"), "{err}");
    }

    // ---- cfg evaluation (§2.2) ---------------------------------------------

    #[test]
    fn cfg_groups_project_by_truth() {
        let tmp = TempDir::new().unwrap();
        for f in ["neutral.cpp", "posix.cpp", "linux_only.cpp", "win.cpp"] {
            touch(tmp.path(), f);
        }
        let mut t = target(TargetKind::Executable, &["neutral.cpp"]);
        let unix_group = crate::schema::TargetCfgGroup {
            sources: strings(&["posix.cpp"]),
            ..Default::default()
        };
        let linux_group = crate::schema::TargetCfgGroup {
            sources: strings(&["linux_only.cpp"]),
            defines: vis(&[], &["USE_PPOLL"]),
            ..Default::default()
        };
        // Deliberately name a file that does NOT exist: non-matching groups
        // are never expanded or path-checked.
        let win_group = crate::schema::TargetCfgGroup {
            sources: strings(&["missing_win32.cpp"]),
            ..Default::default()
        };
        t.cfg = vec![
            (pred(CfgAtom::Unix), unix_group),
            (pred(CfgAtom::Linux), linux_group),
            (pred(CfgAtom::Windows), win_group),
        ];
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));

        // macOS truth: unix matches, linux/windows do not.
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let sources: Vec<String> = find(&plan, "app")
            .units
            .iter()
            .map(|u| u.source.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(sources, vec!["neutral.cpp", "posix.cpp"]);
        assert!(!find(&plan, "app").units[0]
            .defines
            .iter()
            .any(|(k, _)| k == "USE_PPOLL"));

        // Linux truth: unix AND linux match, in document order.
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &linux_truth(),
            &BuildRequest::Default,
        )
        .unwrap();
        let sources: Vec<String> = find(&plan, "app")
            .units
            .iter()
            .map(|u| u.source.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(sources, vec!["neutral.cpp", "posix.cpp", "linux_only.cpp"]);
        assert!(find(&plan, "app").units[0]
            .defines
            .iter()
            .any(|(k, _)| k == "USE_PPOLL"));
    }

    #[test]
    fn zero_sources_error_names_non_matching_groups() {
        let tmp = TempDir::new().unwrap();
        let mut t = target(TargetKind::Executable, &[]);
        let win_group = crate::schema::TargetCfgGroup {
            sources: strings(&["win.cpp"]),
            ..Default::default()
        };
        t.cfg = vec![(pred(CfgAtom::Windows), win_group)];
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("no sources"), "{err}");
        assert!(err.contains("windows"), "{err}");
    }

    #[test]
    fn reference_to_cfgd_out_dep_names_predicate() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["winreg::winreg"], &[]);
        let mut winreg = dep();
        winreg.cfg = Some(pred(CfgAtom::Windows));
        let p = project(
            BTreeMap::from([("winreg".to_string(), winreg)]),
            BTreeMap::from([("app".to_string(), app)]),
        );
        // winreg is cfg'd out on macOS: no manifest provided.
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("behind cfg 'windows'"), "{err}");
        assert!(err.contains("false for this toolchain"), "{err}");
    }

    #[test]
    fn cfg_conditional_target_dependency_edge() {
        // A dependency edge added under cfg.unix is followed on macOS and
        // ignored under a windows-truth... (windows truth is inexpressible
        // here — assert the macos side and the linux side agree).
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        let unix_group = crate::schema::TargetCfgGroup {
            dependencies: vis(&["posixlib::posixlib"], &[]),
            ..Default::default()
        };
        app.cfg = vec![(pred(CfgAtom::Unix), unix_group)];
        let p = project(
            BTreeMap::from([("posixlib".to_string(), dep())]),
            BTreeMap::from([("app".to_string(), app)]),
        );
        let manifests = BTreeMap::from([(
            "posixlib".to_string(),
            manifest("posixlib", &[("posixlib::posixlib", archive("/store/p/libp.a"))]),
        )]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(archive_paths(find(&plan, "app")), vec!["/store/p/libp.a"]);
    }

    // ---- flag layering (§1.3) ----------------------------------------------

    #[test]
    fn compile_flag_layering_order() {
        // [flags] -> profile -> propagated public (dep target) -> own public
        // -> own private, last-wins by position.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "lib.cpp");
        let mut lib = target(TargetKind::StaticLibrary, &["lib.cpp"]);
        lib.cxx_flags = vis(&["-fno-exceptions"], &["-Wall"]);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["lib"], &[]);
        app.cxx_flags = vis(&["-fapp-public"], &["-fapp-private"]);
        let mut p = project(
            BTreeMap::new(),
            BTreeMap::from([("app".to_string(), app), ("lib".to_string(), lib)]),
        );
        p.flags.cxx_flags = vec!["-fpkg".to_string()];
        let profile = Profile {
            cxx_flags: vec!["-fprofile".to_string()],
            c_flags: vec![],
            link_flags: vec![],
        };
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &profile,
            &macos_truth(),
            &BuildRequest::Default,
        )
        .unwrap();
        let app = find(&plan, "app");
        let flags = &app.units[0].extra_flags;
        let pos = |w: &str| {
            flags
                .iter()
                .position(|f| f == w)
                .unwrap_or_else(|| panic!("missing {w} in {flags:?}"))
        };
        assert!(pos("-fpkg") < pos("-fprofile"), "{flags:?}");
        assert!(pos("-fprofile") < pos("-fno-exceptions"), "{flags:?}");
        assert!(pos("-fno-exceptions") < pos("-fapp-public"), "{flags:?}");
        assert!(pos("-fapp-public") < pos("-fapp-private"), "{flags:?}");
        // lib's PRIVATE -Wall must not propagate.
        assert!(!flags.contains(&"-Wall".to_string()), "{flags:?}");
        // lib's own unit sees public then private.
        let lib = find(&plan, "lib");
        let lflags = &lib.units[0].extra_flags;
        let lp = |w: &str| lflags.iter().position(|f| f == w).unwrap();
        assert!(lp("-fno-exceptions") < lp("-Wall"), "{lflags:?}");
        // Export metadata carries the public bucket only.
        assert_eq!(lib.public_flags, vec!["-fno-exceptions".to_string()]);
    }

    #[test]
    fn flags_cfg_groups_and_abi_stripping() {
        // [flags.cfg.<matching>] appends after unconditional entries; ABI
        // words are stripped from the [flags] layer (cli injects them).
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let app = target(TargetKind::Executable, &["main.cpp"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.flags.cxx_flags = vec!["-fpkg".to_string(), "-stdlib=libc++".to_string()];
        p.flags.cfg = vec![
            (
                pred(CfgAtom::Clang),
                crate::schema::PackageFlagsGroup {
                    cxx_flags: vec!["-fclang-only".to_string()],
                    c_flags: vec![],
                    link_flags: vec![],
                },
            ),
            (
                pred(CfgAtom::Gcc),
                crate::schema::PackageFlagsGroup {
                    cxx_flags: vec!["-fgcc-only".to_string()],
                    c_flags: vec![],
                    link_flags: vec![],
                },
            ),
        ];
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let flags = &find(&plan, "app").units[0].extra_flags;
        assert!(flags.contains(&"-fpkg".to_string()), "{flags:?}");
        assert!(flags.contains(&"-fclang-only".to_string()), "{flags:?}");
        assert!(!flags.contains(&"-fgcc-only".to_string()), "{flags:?}");
        assert!(
            !flags.contains(&"-stdlib=libc++".to_string()),
            "ABI words leave layer 3 (cli injects them at layer 2): {flags:?}"
        );
    }

    #[test]
    fn propagated_flags_dedup_by_contributor_not_string() {
        // Diamond: two DIFFERENT components each contribute -pthread; the
        // word appears twice (dedup is by contributing target only).
        let mut a_c = archive("/store/a/liba.a");
        a_c.compile_options = vec!["-pthread".to_string()];
        let mut b_c = archive("/store/b/libb.a");
        b_c.compile_options = vec!["-pthread".to_string()];
        let deps = BTreeMap::from([("a".to_string(), dep()), ("b".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["a::a", "b::b"]);
        let manifests = BTreeMap::from([
            ("a".to_string(), manifest("a", &[("a::a", a_c.clone())])),
            ("b".to_string(), manifest("b", &[("b::b", b_c)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let count = find(&plan, "app").units[0]
            .extra_flags
            .iter()
            .filter(|f| *f == "-pthread")
            .count();
        assert_eq!(count, 2, "one contribution per contributor");

        // Same component reached twice (diamond) contributes ONCE.
        let mut mid1 = archive("/store/m1/libm1.a");
        mid1.requires = vec!["a::a".to_string()];
        let mut mid2 = archive("/store/m2/libm2.a");
        mid2.requires = vec!["a::a".to_string()];
        let deps = BTreeMap::from([
            ("m1".to_string(), dep()),
            ("m2".to_string(), dep()),
            ("a".to_string(), dep()),
        ]);
        let (p, tmp) = exe_project(deps, &["m1::m1", "m2::m2"]);
        let manifests = BTreeMap::from([
            ("m1".to_string(), manifest("m1", &[("m1::m1", mid1)])),
            ("m2".to_string(), manifest("m2", &[("m2::m2", mid2)])),
            ("a".to_string(), manifest("a", &[("a::a", a_c)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let count = find(&plan, "app").units[0]
            .extra_flags
            .iter()
            .filter(|f| *f == "-pthread")
            .count();
        assert_eq!(count, 1, "diamonds contribute once");
    }

    #[test]
    fn profile_flags_route_by_language() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.cpp");
        touch(tmp.path(), "b.c");
        let t = target(TargetKind::Executable, &["a.cpp", "b.c"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let profile = Profile {
            cxx_flags: vec!["-fcxx-only".to_string()],
            c_flags: vec!["-fc-only".to_string()],
            link_flags: vec!["-Wl,-dead_strip".to_string()],
        };
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &profile,
            &macos_truth(),
            &BuildRequest::Default,
        )
        .unwrap();
        let app = find(&plan, "app");
        let cpp = app.units.iter().find(|u| u.lang == Lang::Cxx).unwrap();
        let c = app.units.iter().find(|u| u.lang == Lang::C).unwrap();
        assert!(cpp.extra_flags.contains(&"-fcxx-only".to_string()));
        assert!(!cpp.extra_flags.contains(&"-fc-only".to_string()));
        assert!(c.extra_flags.contains(&"-fc-only".to_string()));
        assert!(!c.extra_flags.contains(&"-fcxx-only".to_string()));
        assert!(app.link_flags.contains(&"-Wl,-dead_strip".to_string()));
    }

    // ---- Threads::Threads builtin (§5.4) ------------------------------------

    #[test]
    fn threads_builtin_resolves_with_zero_declaration() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&[], &["Threads::Threads"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));

        // Linux: -pthread on compile and link.
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &linux_truth(),
            &BuildRequest::Default,
        )
        .unwrap();
        let app_t = find(&plan, "app");
        assert!(
            app_t.units[0].extra_flags.contains(&"-pthread".to_string()),
            "{:?}",
            app_t.units[0].extra_flags
        );
        assert!(
            app_t
                .link_inputs
                .iter()
                .any(|li| matches!(li, LinkInput::Flag(w) if w == "-pthread")),
            "link line carries -pthread on linux"
        );

        // macOS: nothing.
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let app_t = find(&plan, "app");
        assert!(!app_t.units[0].extra_flags.contains(&"-pthread".to_string()));
        assert!(!app_t
            .link_inputs
            .iter()
            .any(|li| matches!(li, LinkInput::Flag(w) if w == "-pthread")));
    }

    #[test]
    fn threads_builtin_reachable_through_component_requires() {
        // A manifest component requiring the rewritten `builtin:threads`
        // link input resolves through ladder step 0 like any reference.
        let mut sync = archive("/store/absl/libsync.a");
        sync.requires = vec!["builtin:threads".to_string()];
        let deps = BTreeMap::from([("absl".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["absl::sync"]);
        let manifests =
            BTreeMap::from([("absl".to_string(), manifest("absl", &[("absl::sync", sync)]))]);
        let plan = plan_full(
            &p,
            tmp.path(),
            &manifests,
            &Profile::default(),
            &linux_truth(),
            &BuildRequest::Default,
        )
        .unwrap();
        let app = find(&plan, "app");
        assert!(app.units[0].extra_flags.contains(&"-pthread".to_string()));
        assert!(app
            .link_inputs
            .iter()
            .any(|li| matches!(li, LinkInput::Flag(w) if w == "-pthread")));
    }

    // ---- system-includes (§1.1 / A.1) ---------------------------------------

    #[test]
    fn dep_system_includes_opt_out_forces_plain_i() {
        let mut c = archive("/store/f/libf.a");
        c.system_includes = vec![PathBuf::from("/store/f/include")];
        let mut f_dep = dep();
        f_dep.system_includes = Some(false);
        let deps = BTreeMap::from([("f".to_string(), f_dep)]);
        let (p, tmp) = exe_project(deps, &["f::f"]);
        let manifests = BTreeMap::from([("f".to_string(), manifest("f", &[("f::f", c)]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let unit = &find(&plan, "app").units[0];
        let entry = unit
            .includes
            .iter()
            .find(|(p, _)| p == &PathBuf::from("/store/f/include"))
            .unwrap();
        assert!(!entry.1, "system-includes = false forces -I");
    }

    #[test]
    fn project_target_system_includes_moves_consumer_view() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "vendored.cpp");
        let mut vendored = target(TargetKind::StaticLibrary, &["vendored.cpp"]);
        vendored.includes = vis(&["vendor/include"], &[]);
        vendored.system_includes = Some(true);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["vendored"], &[]);
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("app".to_string(), app), ("vendored".to_string(), vendored)]),
        );
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let vdir = tmp.path().join("vendor/include");
        // Consumer sees -isystem...
        let app_unit = &find(&plan, "app").units[0];
        assert!(app_unit.includes.iter().any(|(p, sys)| p == &vdir && *sys));
        // ...the target's own TUs see -I.
        let own_unit = &find(&plan, "vendored").units[0];
        assert!(own_unit.includes.iter().any(|(p, sys)| p == &vdir && !*sys));
    }

    // ---- generate steps (§4.2) ----------------------------------------------

    fn gen_step_command(argv: &[&str], stdout: &str, inputs: &[&str]) -> GenerateStep {
        GenerateStep {
            name: String::new(), // filled by key in real loads; unused here
            action: GenerateAction::Command {
                argv: strings(argv),
                stdin: None,
                stdout: stdout.to_string(),
            },
            inputs: strings(inputs),
            checked_in: None,
        }
    }

    #[test]
    fn gen_steps_activate_lazily_and_order_by_edges() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "scripts/gen_a.py");
        touch(tmp.path(), "scripts/gen_b.py");
        // b consumes a's output; a target includes ${gen} -> both activate,
        // a before b. An unrelated dormant step (missing input!) stays out.
        let step_a = gen_step_command(&["python3", "scripts/gen_a.py"], "a.h", &["scripts/gen_a.py"]);
        let mut step_b =
            gen_step_command(&["python3", "scripts/gen_b.py"], "b.h", &["scripts/gen_b.py"]);
        step_b.inputs.push("${gen}/a.h".to_string());
        let dormant = gen_step_command(&["sh", "missing.sh"], "dormant/never.h", &["missing.sh"]);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.includes = vis(&[], &["${gen}"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.generate = BTreeMap::from([
            ("gen-a".to_string(), step_a),
            ("gen-b".to_string(), step_b),
        ]);
        // Dormant step in a separate project to prove input validation is
        // scoped to the activated set: here it coexists but nothing under
        // dormant/ is referenced... it still activates via the bare ${gen}
        // include prefix, so give it its own test below. Remove it:
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let names: Vec<&str> = plan.gen_steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["gen-a", "gen-b"], "producer before consumer");
        assert!(find(&plan, "app").units[0].references_gen);

        // Narrow include prefix: only the step under gen/sub activates.
        let mut p2 = p.clone();
        p2.generate.insert("dormant".to_string(), dormant);
        p2.targets.get_mut("app").unwrap().includes = vis(&[], &["${gen}/a.h"]);
        // (a.h as prefix matches only gen-a's output exactly)
        let plan = run_plan(&p2, tmp.path(), &BTreeMap::new()).unwrap();
        let names: Vec<&str> = plan.gen_steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["gen-a"], "dormant step (missing input) untouched");
    }

    #[test]
    fn activated_step_with_missing_input_errors_naming_path() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let step = gen_step_command(&["sh", "gen.sh"], "out.h", &["data/tzdata.zi"]);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.includes = vis(&[], &["${gen}"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.generate = BTreeMap::from([("zones".to_string(), step)]);
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("data/tzdata.zi"), "{err}");
        assert!(err.contains("zones"), "{err}");
    }

    #[test]
    fn gen_source_must_match_declared_output() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "gen.sh");
        let step = gen_step_command(&["sh", "gen.sh"], "browse_py.cc", &["gen.sh"]);
        let mut app = target(TargetKind::Executable, &["${gen}/browse_py.cc"]);
        app.includes = VisibilitySplit::default();
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.generate = BTreeMap::from([("browse".to_string(), step)]);
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let app_t = find(&plan, "app");
        assert_eq!(app_t.units.len(), 1);
        assert!(app_t.units[0].references_gen);
        assert!(app_t.units[0]
            .source
            .ends_with("build/gen/browse_py.cc"));
        assert_eq!(plan.gen_steps.len(), 1);

        // Unmatched ${gen} source is a plan-time hard error.
        let mut p_bad = p.clone();
        p_bad.targets.get_mut("app").unwrap().sources = strings(&["${gen}/nope.cc"]);
        let err = run_plan(&p_bad, tmp.path(), &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("matches no declared"), "{err}");
        assert!(err.contains("browse_py.cc"), "{err}");
    }

    #[test]
    fn checked_in_steps_stay_outside_the_build_graph() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "include/known_zones.h");
        let mut step = gen_step_command(&["python3", "gen.py"], "known_zones.h", &["gen.py"]);
        step.checked_in = Some("include/known_zones.h".to_string());
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.includes = vis(&[], &["${gen}"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.generate = BTreeMap::from([("zones".to_string(), step)]);
        // Even with a ${gen} include in play, the checked-in step never
        // activates (its declared input gen.py is absent — no error).
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert!(plan.gen_steps.is_empty());

        // Referencing its output via ${gen} is an error with the fix.
        let mut p_bad = p.clone();
        p_bad.targets.get_mut("app").unwrap().sources = strings(&["${gen}/known_zones.h"]);
        let err = run_plan(&p_bad, tmp.path(), &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("checked-in"), "{err}");
        assert!(err.contains("include/known_zones.h"), "{err}");
    }

    #[test]
    fn gen_step_cycle_is_error() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let mut a = gen_step_command(&["sh", "a.sh"], "a.h", &[]);
        a.inputs = strings(&["${gen}/b.h"]);
        let mut b = gen_step_command(&["sh", "b.sh"], "b.h", &[]);
        b.inputs = strings(&["${gen}/a.h"]);
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.includes = vis(&[], &["${gen}"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.generate = BTreeMap::from([("a".to_string(), a), ("b".to_string(), b)]);
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn template_vars_interpolate_package_version() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        touch(tmp.path(), "version.hpp.in");
        let step = GenerateStep {
            name: String::new(),
            action: GenerateAction::Template {
                template: "version.hpp.in".to_string(),
                output: "version.hpp".to_string(),
                vars: BTreeMap::from([(
                    "PROJECT_VERSION".to_string(),
                    "${package.version}".to_string(),
                )]),
            },
            inputs: vec![],
            checked_in: None,
        };
        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.includes = vis(&[], &["${gen}"]);
        let mut p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), app)]));
        p.package.version = Some("1.9.5".to_string());
        p.generate = BTreeMap::from([("version-header".to_string(), step)]);
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(plan.gen_steps.len(), 1);
        match &plan.gen_steps[0].action {
            GenerateAction::Template { vars, .. } => {
                assert_eq!(vars["PROJECT_VERSION"], "1.9.5");
            }
            other => panic!("expected template action, got {other:?}"),
        }
    }

    // ---- runtime-data staging (§6.5) ----------------------------------------

    #[test]
    fn runtime_data_stages_with_byte_equal_dedupe() {
        let tmp = TempDir::new().unwrap();
        for f in ["a.cpp", "b.cpp"] {
            touch(tmp.path(), f);
        }
        fs::create_dir_all(tmp.path().join("cfg")).unwrap();
        fs::write(tmp.path().join("cfg/std.cfg"), "content").unwrap();
        fs::write(tmp.path().join("cfg/skip-unsigned.xml"), "x").unwrap();
        let rd = RuntimeData {
            from: "cfg".to_string(),
            patterns: strings(&["*.cfg", "*.xml", "!*-unsigned.xml"]),
            to: "cfg".to_string(),
        };
        let mut a = target(TargetKind::Executable, &["a.cpp"]);
        a.runtime_data = vec![rd.clone()];
        let mut b = target(TargetKind::Executable, &["b.cpp"]);
        b.runtime_data = vec![rd];
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("cppcheck".to_string(), a), ("testrunner".to_string(), b)]),
        );
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(plan.data_stages.len(), 1, "{:?}", plan.data_stages);
        let stage = &plan.data_stages[0];
        assert_eq!(stage.dest, PathBuf::from("cfg/std.cfg"));
        assert_eq!(
            stage.for_targets,
            vec!["cppcheck".to_string(), "testrunner".to_string()],
            "both declarers share the staged copy"
        );
        assert!(stage.src.is_absolute());
    }

    #[test]
    fn runtime_data_conflicting_bytes_is_error() {
        let tmp = TempDir::new().unwrap();
        for f in ["a.cpp", "b.cpp"] {
            touch(tmp.path(), f);
        }
        fs::create_dir_all(tmp.path().join("d1")).unwrap();
        fs::create_dir_all(tmp.path().join("d2")).unwrap();
        fs::write(tmp.path().join("d1/x.cfg"), "one").unwrap();
        fs::write(tmp.path().join("d2/x.cfg"), "two").unwrap();
        let mut a = target(TargetKind::Executable, &["a.cpp"]);
        a.runtime_data = vec![RuntimeData {
            from: "d1".to_string(),
            patterns: strings(&["**/*"]),
            to: "cfg".to_string(),
        }];
        let mut b = target(TargetKind::Executable, &["b.cpp"]);
        b.runtime_data = vec![RuntimeData {
            from: "d2".to_string(),
            patterns: strings(&["**/*"]),
            to: "cfg".to_string(),
        }];
        let p = project(
            BTreeMap::new(),
            BTreeMap::from([("a".to_string(), a), ("b".to_string(), b)]),
        );
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("different bytes"), "{err}");
    }

    #[test]
    fn runtime_data_missing_from_dir_is_error() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "a.cpp");
        let mut a = target(TargetKind::Executable, &["a.cpp"]);
        a.runtime_data = vec![RuntimeData {
            from: "nodir".to_string(),
            patterns: vec![],
            to: "nodir".to_string(),
        }];
        let p = project(BTreeMap::new(), BTreeMap::from([("a".to_string(), a)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("nodir"), "{err}");
        assert!(err.contains("not a directory"), "{err}");
    }

    // ---- run entries ---------------------------------------------------------

    #[test]
    fn run_entries_interpolate_and_carry_through() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "t.cpp");
        let mut t = target(TargetKind::Executable, &["t.cpp"]);
        t.test = true;
        t.dev = true;
        t.run = vec![RunEntry {
            name: Some("death-bad-env".to_string()),
            args: strings(&["--root", "${project-root}"]),
            cwd: Some("tzdb-runtime".to_string()),
            env: BTreeMap::from([(
                "VTZ_TZDATA_PATH".to_string(),
                "${gen}/zoneinfo".to_string(),
            )]),
            env_remove: strings(&["TZ"]),
            expect_failure: true,
        }];
        let p = project(BTreeMap::new(), BTreeMap::from([("t".to_string(), t)]));
        let plan = plan_full(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            &Profile::default(),
            &macos_truth(),
            &BuildRequest::Tests(vec![]),
        )
        .unwrap();
        let t = find(&plan, "t");
        assert_eq!(t.run.len(), 1);
        let e = &t.run[0];
        assert_eq!(e.args[1], tmp.path().display().to_string());
        assert!(e.env["VTZ_TZDATA_PATH"].ends_with("zoneinfo"));
        assert!(!e.env["VTZ_TZDATA_PATH"].contains("${gen}"));
        assert!(e.expect_failure);
        assert_eq!(e.env_remove, vec!["TZ".to_string()]);
    }

    // ---- required_deps (provisioning laziness) -------------------------------

    #[test]
    fn required_deps_laziness_matrix() {
        let mut p = project(BTreeMap::new(), BTreeMap::new());
        // regular fetched dep: always in (when cfg-active)
        p.dependencies.insert("fmt".to_string(), dep());
        // cfg'd-out dep: never in on macOS
        let mut win = dep();
        win.cfg = Some(pred(CfgAtom::Windows));
        p.dependencies.insert("winreg".to_string(), win);
        // dev-dep: only when a dev/test target is selected
        let mut gtest = dep();
        gtest.dev = true;
        gtest.find_package = Some("GTest".to_string());
        p.dev_dependencies.insert("googletest".to_string(), gtest);
        // system dep: only when referenced by the selection
        let mut boost = dep();
        boost.source = SourceSpec::System { min_version: None };
        boost.exposes_namespace = vec!["Boost".to_string()];
        p.dependencies.insert("boost".to_string(), boost);

        let mut app = target(TargetKind::Executable, &["main.cpp"]);
        app.dependencies = vis(&["fmt::fmt"], &[]);
        let mut tests = target(TargetKind::Executable, &["t.cpp"]);
        tests.test = true;
        tests.dev = true;
        tests.dependencies = vis(&["GTest::gtest_main"], &[]);
        let mut boosty = target(TargetKind::Executable, &["b.cpp"]);
        boosty.dependencies = vis(&["Boost::container"], &[]);
        p.targets = BTreeMap::from([
            ("app".to_string(), app),
            ("tests".to_string(), tests),
            ("boosty".to_string(), boosty),
        ]);

        let truth = macos_truth();
        // Default: fmt + boost (boosty is non-dev and references Boost::*);
        // no dev-deps, no cfg'd-out deps.
        let deps = required_deps(&p, &truth, &BuildRequest::Default).unwrap();
        assert_eq!(
            deps,
            BTreeSet::from(["fmt".to_string(), "boost".to_string()])
        );
        // Named(app): fmt only — boost unreferenced by the selection.
        let deps =
            required_deps(&p, &truth, &BuildRequest::Named(vec!["app".to_string()])).unwrap();
        assert_eq!(deps, BTreeSet::from(["fmt".to_string()]));
        // Tests: dev-dep joins; boost stays out.
        let deps = required_deps(&p, &truth, &BuildRequest::Tests(vec![])).unwrap();
        assert_eq!(
            deps,
            BTreeSet::from(["fmt".to_string(), "googletest".to_string()])
        );
    }

    #[test]
    fn required_deps_needs_reaching_cfgd_out_dep_errors() {
        let mut p = project(BTreeMap::new(), BTreeMap::new());
        let mut a = dep();
        a.needs = vec!["b".to_string()];
        let mut b = dep();
        b.cfg = Some(pred(CfgAtom::Windows));
        p.dependencies.insert("a".to_string(), a);
        p.dependencies.insert("b".to_string(), b);
        let err = required_deps(&p, &macos_truth(), &BuildRequest::Default)
            .unwrap_err()
            .to_string();
        assert!(err.contains("behind cfg 'windows'"), "{err}");
    }

    // ---- export metadata ------------------------------------------------------

    #[test]
    fn planned_target_carries_export_metadata() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "lib.cpp");
        let mut lib = target(TargetKind::StaticLibrary, &["lib.cpp"]);
        lib.install = true;
        lib.cxx_std = Some(17);
        lib.includes = vis(&["include/api"], &["include/impl"]);
        lib.defines = vis(&["VTZ_API=1"], &["VTZ_INTERNAL"]);
        lib.cxx_flags = vis(&["-fno-exceptions"], &["-Werror"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("vtz".to_string(), lib)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let t = find(&plan, "vtz");
        assert!(t.install);
        assert!(!t.dev && !t.test);
        assert_eq!(t.public_includes, vec![tmp.path().join("include/api")]);
        assert_eq!(
            t.public_defines,
            vec![("VTZ_API".to_string(), Some("1".to_string()))]
        );
        assert_eq!(t.public_flags, vec!["-fno-exceptions".to_string()]);
        assert_eq!(t.cxx_std, Some(17));
    }

    #[test]
    fn external_deps_grouped_by_key() {
        let mut spd = archive("/store/spdlog/libspdlog.a");
        spd.requires = vec!["fmt::fmt".to_string()];
        let fmt_c = archive("/store/fmt/libfmt.a");
        let deps = BTreeMap::from([("fmt".to_string(), dep()), ("spdlog".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["spdlog::spdlog"]);
        let manifests = BTreeMap::from([
            ("fmt".to_string(), manifest("fmt", &[("fmt::fmt", fmt_c)])),
            ("spdlog".to_string(), manifest("spdlog", &[("spdlog::spdlog", spd)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let ext = &find(&plan, "app").external_deps;
        assert_eq!(ext["spdlog"], vec!["spdlog::spdlog".to_string()]);
        assert_eq!(ext["fmt"], vec!["fmt::fmt".to_string()]);
    }

    // ---- cross-package requires (find_dependency chains) -------------------

    #[test]
    fn cross_package_requires_resolve_through_ladder() {
        let mut spd = archive("/store/spdlog/libspdlog.a");
        spd.includes = vec![PathBuf::from("/store/spdlog/include")];
        spd.requires = vec!["fmt::fmt".to_string()];
        let mut fmt_real = archive("/store/fmt/libfmt.a");
        fmt_real.includes = vec![PathBuf::from("/store/fmt/include")];
        let fmt_shadow = archive("/store/spdlog/libfmt-shadow.a");
        let deps = BTreeMap::from([("fmt".to_string(), dep()), ("spdlog".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["spdlog::spdlog"]);
        let manifests = BTreeMap::from([
            ("fmt".to_string(), manifest("fmt", &[("fmt::fmt", fmt_real)])),
            (
                "spdlog".to_string(),
                manifest("spdlog", &[("spdlog::spdlog", spd), ("fmt::fmt", fmt_shadow)]),
            ),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        assert!(include_dirs(&app.units[0]).contains(&"/store/fmt/include".to_string()));
        assert_eq!(
            archive_paths(app),
            vec!["/store/spdlog/libspdlog.a", "/store/fmt/libfmt.a"]
        );
    }

    #[test]
    fn component_link_requires_is_link_only() {
        let mut core = archive("/store/core/libcore.a");
        core.link_requires = vec!["impl::impl".to_string()];
        let mut impl_c = archive("/store/impl/libimpl.a");
        impl_c.includes = vec![PathBuf::from("/store/impl/include")];
        let deps = BTreeMap::from([("core".to_string(), dep()), ("impl".to_string(), dep())]);
        let (p, tmp) = exe_project(deps, &["core::core"]);
        let manifests = BTreeMap::from([
            ("core".to_string(), manifest("core", &[("core::core", core)])),
            ("impl".to_string(), manifest("impl", &[("impl::impl", impl_c)])),
        ]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let app = find(&plan, "app");
        assert!(!include_dirs(&app.units[0]).contains(&"/store/impl/include".to_string()));
        assert_eq!(
            archive_paths(app),
            vec!["/store/core/libcore.a", "/store/impl/libimpl.a"]
        );
    }

    // ---- static library plan ----------------------------------------------

    #[test]
    fn static_library_plan_archives_own_objects_only() {
        let (p, tmp, manifests) = visibility_fixture();
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        let core = find(&plan, "core");
        assert_eq!(core.output, PathBuf::from("libcore.a"));
        assert_eq!(core.link_inputs.len(), 1);
        assert!(matches!(&core.link_inputs[0], LinkInput::Object(p) if p == &PathBuf::from("core.dir/src/core.cpp.o")));
        assert!(core.link_flags.is_empty());
        assert_eq!(find(&plan, "myapp").output, PathBuf::from("myapp"));
        assert_eq!(find(&plan, "myapp").target_deps, vec!["core".to_string()]);
    }
}
