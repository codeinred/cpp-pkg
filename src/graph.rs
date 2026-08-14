//! Target-graph resolution and build planning.
//!
//! Responsibilities (CPPKG_TOML.md "Semantics"):
//! 1. NAMING LADDER for every dependency reference in [targets.*]:
//!    unique across manifests -> direct; else `<depkey>::` prefix owns; else
//!    exposes-namespace / exposes-targets (mapping form renames); else HARD
//!    ERROR listing candidate owning packages + the exposes-* fix.
//!    Local target names (no "::") resolve to sibling targets.
//! 2. VISIBILITY PROPAGATION: public deps/includes/defines propagate to
//!    consumers; private do not — EXCEPT private deps of a static-library
//!    propagate as LINK-ONLY edges (artifacts reach the final link closure,
//!    compile requirements stop). Manifest `requires` are public edges of
//!    that component; `link_requires` are link-only.
//! 3. SOURCES: expand globs relative to the project root in sorted byte
//!    order. Extension table (exhaustive, hard error otherwise):
//!    .cpp .cc .cxx .c++ -> C++ | .c -> C | .C -> error (case-insensitive
//!    FS) | .m .mm -> error ("Objective-C not supported in v0").
//! 4. INTERFACE_SOURCES of consumed components become CompileUnits of the
//!    consuming target (compiled with that component's usage requirements).
//! 5. LINK PLAN: topological order over the closure; static archives
//!    deduped keeping the LAST occurrence; frameworks/system libs deduped
//!    keeping first. Cycles among manifest components -> error (v0; group
//!    support later).
//! 6. LINK LANGUAGE: any C++ unit in the target or C++ anywhere in its
//!    closure -> C++ driver links.
//! 7. cxx-std: per-target `cxx-std` max-merged with the max `cxx_std`
//!    required by consumed components.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};

use crate::manifest::{Component, ComponentKind, Manifest};
use crate::schema::{BuildConfig, ProjectFile, TargetKind};
use crate::toolchain::Lang;
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
}

#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// Topological order (dependencies first).
    pub targets: Vec<PlannedTarget>,
}

/// Resolve + plan. `manifests` is keyed by dependency key. `only` restricts
/// to the named targets (+ their transitive sibling deps); empty = all.
pub fn plan(
    project: &ProjectFile,
    project_root: &std::path::Path,
    manifests: &BTreeMap<String, Manifest>,
    config: BuildConfig,
    profile_flags: &crate::schema::Profile,
    only: &[String],
) -> Result<BuildPlan> {
    let mut planner = Planner {
        project,
        root: project_root,
        manifests,
        config,
        profile: profile_flags,
        exposed: BTreeMap::new(),
        resolve_cache: BTreeMap::new(),
        edge_cache: BTreeMap::new(),
        comp_reqs_cache: BTreeMap::new(),
    };
    planner.build_exposed_table()?;
    let selected = planner.select(only)?;

    // Pass 1: compile units for every selected target. Link planning needs
    // sibling units to already exist (link-language rule inspects them), so
    // units are computed for the whole selection before any link plan.
    let mut units_map: BTreeMap<String, Vec<CompileUnit>> = BTreeMap::new();
    for name in &selected {
        let closure = planner.closure_nodes(name, false)?;
        let units = planner.compile_units(name, &closure)?;
        units_map.insert(name.clone(), units);
    }

    // Pass 2: link plans + assembly.
    let mut planned: BTreeMap<String, PlannedTarget> = BTreeMap::new();
    for name in &selected {
        let spec = &project.targets[name];
        let units = units_map[name].clone();
        let edges = planner.edges(name)?;

        let mut target_deps: Vec<String> = Vec::new();
        for n in edges.public.iter().chain(edges.private.iter()) {
            if let Node::Sibling(s) = n
                && !target_deps.contains(s) {
                    target_deps.push(s.clone());
                }
        }

        // The link-language rule looks at the full any-edge closure: a
        // private (link-only) C++ archive still forces a C++ link.
        let any_closure = planner.closure_nodes(name, true)?;
        let link_lang = link_language(&units, &any_closure, &units_map);

        let (link_inputs, link_flags) = match spec.kind {
            // A static library does not link; its "link inputs" are the
            // objects the archiver packs.
            TargetKind::StaticLibrary => (
                units
                    .iter()
                    .map(|u| LinkInput::Object(u.object.clone()))
                    .collect(),
                Vec::new(),
            ),
            TargetKind::Executable => planner.link_plan(name, &units)?,
        };

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
            },
        );
    }

    let order = planner.topo(&selected)?;
    Ok(BuildPlan {
        targets: order
            .into_iter()
            .map(|n| planned.remove(&n).expect("topo order covers selection"))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Internal machinery
// ---------------------------------------------------------------------------

/// A node in the dependency graph: a sibling target of this project or a
/// component owned by a dependency package (post-attribution).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Sibling(String),
    Comp { pkg: String, name: String },
}

impl Node {
    fn describe(&self) -> String {
        match self {
            Node::Sibling(s) => format!("{s} (project target)"),
            Node::Comp { pkg, name } => format!("{name} (package {pkg})"),
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
    profile: &'a crate::schema::Profile,
    /// exposed name -> [(dep key, extracted component name)]
    exposed: BTreeMap<String, Vec<(String, String)>>,
    resolve_cache: BTreeMap<String, Node>,
    edge_cache: BTreeMap<String, Edges>,
    comp_reqs_cache: BTreeMap<(String, String), Reqs>,
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
/// object tree from colliding with the target's own output — an executable
/// named `hello` would otherwise fight a directory named `hello`. Sources
/// outside the project root (interface sources living in the store) get a
/// path-hash prefix instead of a relpath so two distinct external sources
/// with the same file name cannot collide.
fn object_path(target: &str, root: &Path, src: &Path) -> PathBuf {
    let obj_root = PathBuf::from(format!("{target}.dir"));
    // The lexical strip only helps when the result stays inside the object
    // tree: `..`/`.` segments (literal entries like `src/../../x.cpp` strip
    // cleanly but then escape `<target>.dir/`, risking writes outside the
    // build dir and cross-target collisions) take the hashed branch instead.
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

/// Expand a target's source patterns. Globs (patterns containing `*?[`)
/// resolve in sorted lexicographic byte order and may match nothing; literal
/// paths must exist. Duplicates dedup keeping the first occurrence.
fn expand_sources(target: &str, patterns: &[String], root: &Path) -> Result<Vec<PathBuf>> {
    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("project root `{}` is not valid UTF-8", root.display()))?;
    let mut out: Vec<PathBuf> = Vec::new();
    for pat in patterns {
        if pat.contains(['*', '?', '[']) {
            // Escape the root so glob metacharacters in the directory path
            // itself (e.g. brackets in a temp dir name) stay literal.
            let full = format!("{}/{}", glob::Pattern::escape(root_str), pat);
            let paths = glob::glob(&full)
                .map_err(|e| anyhow!("target `{target}`: invalid glob `{pat}`: {e}"))?;
            let mut matches: Vec<PathBuf> = Vec::new();
            for entry in paths {
                let p = entry.map_err(|e| anyhow!("target `{target}`: glob `{pat}`: {e}"))?;
                if p.is_file() {
                    matches.push(p);
                }
            }
            matches.sort_by(|a, b| {
                a.as_os_str()
                    .as_encoded_bytes()
                    .cmp(b.as_os_str().as_encoded_bytes())
            });
            out.extend(matches);
        } else {
            let p = root.join(pat);
            if !p.is_file() {
                bail!(
                    "target `{target}`: source `{pat}` not found (looked at {})",
                    p.display()
                );
            }
            out.push(p);
        }
    }
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    out.retain(|p| seen.insert(p.clone()));
    Ok(out)
}

/// Extra flags for one unit: interface compile options of consumed
/// components, then the profile's per-language flags (consumer targets only;
/// language routing per CPPKG_TOML.md).
fn unit_flags(options: &[String], profile: &crate::schema::Profile, lang: Lang) -> Vec<String> {
    let mut flags: Vec<String> = options.to_vec();
    match lang {
        Lang::Cxx => flags.extend(profile.cxx_flags.iter().cloned()),
        Lang::C => flags.extend(profile.c_flags.iter().cloned()),
    }
    flags
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

/// Rule-5 dedup: archives keep the LAST occurrence (symbol resolution walks
/// left to right; the last position satisfies every earlier referencer);
/// everything else keeps the first.
fn dedup_link_inputs(raw: Vec<LinkInput>) -> Vec<LinkInput> {
    let mut last_archive: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for (i, li) in raw.iter().enumerate() {
        if let LinkInput::Archive(p) = li {
            last_archive.insert(p.clone(), i);
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for (i, li) in raw.into_iter().enumerate() {
        let keep = match &li {
            LinkInput::Archive(p) => last_archive[p] == i,
            LinkInput::Object(p) => seen.insert(format!("obj\u{1f}{}", p.display())),
            LinkInput::Dylib(p) => seen.insert(format!("dylib\u{1f}{}", p.display())),
            LinkInput::SystemLib(n) => seen.insert(format!("sys\u{1f}{n}")),
            LinkInput::Framework(n) => seen.insert(format!("fw\u{1f}{n}")),
        };
        if keep {
            out.push(li);
        }
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

    /// Exposed-name table: every manifest component under its exposed name
    /// (the `exposes-targets` mapping form renames — the extracted name is
    /// then no longer exposed, which is itself a disambiguation mechanism).
    fn build_exposed_table(&mut self) -> Result<()> {
        for (key, manifest) in self.manifests {
            let renames = self
                .project
                .dependencies
                .get(key)
                .map(|d| &d.exposes_targets.renames);
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
        // through to exposes-* claims by other packages: a user writing
        // `foo::bar` to mean dep `foo` must get foo's does-not-export error,
        // not an ambiguity (or silent claim) among unrelated packages.
        if let Some(prefix) = namespace_of(reference)
            && (self.project.dependencies.contains_key(prefix)
                || self.manifests.contains_key(prefix))
        {
            if let Some((pkg, name)) = candidates.iter().find(|(k, _)| k == prefix) {
                return Ok(Node::Comp {
                    pkg: pkg.clone(),
                    name: name.clone(),
                });
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
            bail!(
                "unknown dependency reference `{reference}` ({context}): not a \
                 target of this project and not exported by any dependency"
            );
        }

        // Ladder step 3: exposes-namespace / exposes-targets declarations.
        let claimed: Vec<(String, String)> = candidates
            .iter()
            .filter(|(pkg, extracted)| {
                let Some(dep) = self.project.dependencies.get(pkg) else {
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

    /// Resolve one target's direct dependency edges (cached).
    fn edges(&mut self, tname: &str) -> Result<Edges> {
        if let Some(e) = self.edge_cache.get(tname) {
            return Ok(e.clone());
        }
        let project: &'a ProjectFile = self.project;
        let spec = project
            .targets
            .get(tname)
            .ok_or_else(|| anyhow!("unknown target `{tname}`"))?;
        let context = format!("referenced by target `{tname}`");
        let mut edges = Edges::default();
        for (names, out) in [
            (&spec.dependencies.public, &mut edges.public),
            (&spec.dependencies.private, &mut edges.private),
        ] {
            for r in names {
                let node = self.resolve(r, &context)?;
                if let Node::Sibling(s) = &node
                    && project.targets[s].kind == TargetKind::Executable {
                        bail!("target `{tname}` depends on `{s}`, which is an executable");
                    }
                out.push(node);
            }
        }
        self.edge_cache.insert(tname.to_string(), edges.clone());
        Ok(edges)
    }

    /// Selection: `only` targets plus their transitive sibling dependencies
    /// (all visibilities — a private sibling still has to be built).
    fn select(&mut self, only: &[String]) -> Result<BTreeSet<String>> {
        if only.is_empty() {
            return Ok(self.project.targets.keys().cloned().collect());
        }
        let mut work: Vec<String> = Vec::new();
        for name in only {
            if !self.project.targets.contains_key(name) {
                let known: Vec<&str> =
                    self.project.targets.keys().map(String::as_str).collect();
                bail!(
                    "unknown target `{name}` (targets in this project: {})",
                    known.join(", ")
                );
            }
            work.push(name.clone());
        }
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
        };
        for child in children {
            self.closure_visit(&child, all_edges, order, seen)?;
        }
        Ok(())
    }

    /// Full compile requirements of one component: its own usage
    /// requirements plus (recursively) those of its public `requires`.
    /// Cycles among `requires` are a hard error.
    fn comp_reqs(&mut self, pkg: &str, name: &str, stack: &mut Vec<(String, String)>) -> Result<Reqs> {
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
        let mut reqs = Reqs::default();
        for i in &comp.includes {
            reqs.includes.push((i.clone(), false));
        }
        for i in &comp.system_includes {
            reqs.includes.push((i.clone(), true));
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

    /// Compile units of one target: its own sources (globs expanded, sorted)
    /// plus the interface sources of every component in its compile closure.
    fn compile_units(&mut self, tname: &str, closure: &[Node]) -> Result<Vec<CompileUnit>> {
        let project: &'a ProjectFile = self.project;
        let spec = &project.targets[tname];
        let sources = expand_sources(tname, &spec.sources, self.root)?;

        // The target's own requirement set: own includes/defines (private
        // then public — both apply when compiling the target itself), then
        // each closure node's contribution in first-reach order. Closure
        // nodes contribute only what they PROPAGATE (a sibling's private
        // includes stay its own).
        let mut reqs = Reqs::default();
        for inc in spec.includes.private.iter().chain(spec.includes.public.iter()) {
            reqs.includes.push((self.root.join(inc), false));
        }
        for def in spec.defines.private.iter().chain(spec.defines.public.iter()) {
            reqs.defines.push(parse_define(def));
        }
        for node in closure {
            match node {
                Node::Sibling(s) => {
                    let dep_spec = &project.targets[s];
                    for inc in &dep_spec.includes.public {
                        reqs.includes.push((self.root.join(inc), false));
                    }
                    for def in &dep_spec.defines.public {
                        reqs.defines.push(parse_define(def));
                    }
                }
                Node::Comp { pkg, name } => {
                    let comp = self.component(pkg, name)?;
                    for i in &comp.includes {
                        reqs.includes.push((i.clone(), false));
                    }
                    for i in &comp.system_includes {
                        reqs.includes.push((i.clone(), true));
                    }
                    reqs.defines.extend(comp.defines.iter().cloned());
                    reqs.options.extend(comp.compile_options.iter().cloned());
                    reqs.cxx_std = max_std(reqs.cxx_std, comp.cxx_std);
                }
            }
        }
        reqs.dedup();
        let effective_cxx = max_std(spec.cxx_std, reqs.cxx_std);

        let mut units = Vec::new();
        for src in sources {
            let lang = classify_source(&src)?;
            units.push(CompileUnit {
                lang,
                std: match lang {
                    Lang::Cxx => effective_cxx,
                    Lang::C => spec.c_std,
                },
                includes: reqs.includes.clone(),
                defines: reqs.defines.clone(),
                extra_flags: unit_flags(&reqs.options, self.profile, lang),
                object: object_path(tname, self.root, &src),
                source: src,
            });
        }

        // Interface sources become units of the CONSUMER, compiled with the
        // providing component's usage requirements (not the consumer's own
        // private ones). Two components may inject the same source (e.g. a
        // package exporting `foo` and `foo-headers` with one shared stub);
        // one object rule per output — ninja rejects duplicate outputs — so
        // the first-reached component's requirements win.
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
                units.push(CompileUnit {
                    source: src.clone(),
                    lang,
                    std: match lang {
                        Lang::Cxx => max_std(effective_cxx, creqs.cxx_std),
                        Lang::C => spec.c_std,
                    },
                    includes: creqs.includes.clone(),
                    defines: creqs.defines.clone(),
                    extra_flags: unit_flags(&creqs.options, self.profile, lang),
                    object,
                });
            }
        }

        if units.is_empty() {
            bail!("target `{tname}` has no sources (globs matched nothing)");
        }
        Ok(units)
    }

    /// Rule-5 link plan for an executable: own objects, then a pre-order
    /// walk over ALL edges (public, private, link-only) emitting artifacts
    /// with duplicates, then the archive-keep-last / rest-keep-first dedup.
    /// Pre-order emission puts every dependency after its dependents, which
    /// keep-last preserves through diamonds — the required topological
    /// order for single-pass linkers.
    fn link_plan(
        &mut self,
        tname: &str,
        units: &[CompileUnit],
    ) -> Result<(Vec<LinkInput>, Vec<String>)> {
        let mut raw: Vec<LinkInput> = units
            .iter()
            .map(|u| LinkInput::Object(u.object.clone()))
            .collect();
        let mut opts: Vec<String> = Vec::new();
        let edges = self.edges(tname)?;
        // The executable itself sits on the path stack so an edge cycling
        // back to it is reported as a cycle, not infinite recursion.
        let mut stack = vec![Node::Sibling(tname.to_string())];
        for n in edges.public.iter().chain(edges.private.iter()) {
            self.emit_link(n, &mut raw, &mut opts, &mut stack)?;
        }
        let inputs = dedup_link_inputs(raw);
        let mut seen: BTreeSet<String> = BTreeSet::new();
        opts.retain(|o| seen.insert(o.clone()));
        opts.extend(self.profile.link_flags.iter().cloned());
        Ok((inputs, opts))
    }

    fn emit_link(
        &mut self,
        node: &Node,
        out: &mut Vec<LinkInput>,
        opts: &mut Vec<String>,
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
        match node {
            Node::Sibling(s) => {
                let project: &'a ProjectFile = self.project;
                let spec = &project.targets[s];
                match spec.kind {
                    TargetKind::StaticLibrary => {
                        out.push(LinkInput::Archive(output_path(s, spec.kind)));
                    }
                    TargetKind::Executable => {
                        bail!("target `{s}` is an executable and cannot appear in a link closure")
                    }
                }
                let e = self.edges(s)?;
                // Private deps of a static library propagate as link-only
                // edges: traversed here, invisible to compile propagation.
                for n in e.public.iter().chain(e.private.iter()) {
                    self.emit_link(n, out, opts, stack)?;
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
                match comp.kind {
                    Some(ComponentKind::Archive) => {
                        out.push(LinkInput::Archive(location.ok_or_else(missing)?))
                    }
                    Some(ComponentKind::Dylib) => {
                        out.push(LinkInput::Dylib(location.ok_or_else(missing)?))
                    }
                    Some(ComponentKind::Interface) => {}
                    Some(ComponentKind::Unknown) | None => {
                        if let Some(loc) = location {
                            out.push(classify_artifact(loc));
                        }
                    }
                }
                for p in &comp.link_paths {
                    out.push(classify_artifact(p.clone()));
                }
                for l in &comp.system_libs {
                    out.push(LinkInput::SystemLib(l.clone()));
                }
                for f in &comp.frameworks {
                    out.push(LinkInput::Framework(f.clone()));
                }
                opts.extend(comp.link_options.iter().cloned());
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
                    self.emit_link(&child, out, opts, stack)?;
                }
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
                    && selected.contains(s) {
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
            bail!(
                "dependency cycle among targets: {}",
                remaining.join(" -> ")
            );
        }
        Ok(out)
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
            dependencies: deps,
            targets,
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

    fn run_plan(
        project: &ProjectFile,
        root: &Path,
        manifests: &BTreeMap<String, Manifest>,
    ) -> Result<BuildPlan> {
        plan(
            project,
            root,
            manifests,
            BuildConfig::Release,
            &Profile::default(),
            &[],
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
        // fmt::fmt recorded by BOTH probes (transitive find_dependency);
        // the `fmt::` prefix matches the dep key -> fmt's copy wins.
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
        // Dep key `fmtlib` doesn't match the `fmt::` namespace and another
        // probe also recorded fmt::fmt -> exposes-namespace decides.
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
        // Both deps export zoo::zoo. Dep `a` renames its copy to `zoo-a`,
        // which (1) makes `zoo-a` resolvable and (2) frees `zoo::zoo` to
        // resolve uniquely to dep `b`.
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
        // `foo::bar` names dep `foo`, which does NOT export it; `a` and `b`
        // both do (and `a` even claims the namespace). Step 2 must produce
        // foo's does-not-export error, never an ambiguity among a/b or a
        // silent resolution to a's claim.
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
        // Two components of one package inject the SAME interface source;
        // the consumer must get exactly one CompileUnit for it (duplicate
        // object rules make ninja abort with "multiple rules generate").
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
        // No duplicate object outputs anywhere in the target.
        let mut objs: Vec<_> = app.units.iter().map(|u| u.object.clone()).collect();
        objs.sort();
        objs.dedup();
        assert_eq!(objs.len(), app.units.len());
    }

    #[test]
    fn object_path_with_parent_segments_uses_hashed_branch() {
        // proj/src/../../x.cpp resolves to a real file OUTSIDE proj; the
        // lexical strip would yield app.dir/src/../../x.cpp.o, escaping the
        // object tree — such sources must take the hashed-external branch.
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

    /// myapp(exe) -> core(static): core has PUBLIC fmt::fmt and PRIVATE
    /// spdlog::spdlog. Verifies the whole rule 2 block.
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
        // core's PUBLIC include + fmt's include propagate to myapp...
        assert!(incs.contains(&root.join("include").display().to_string()), "{incs:?}");
        assert!(incs.contains(&"/store/fmt/include".to_string()), "{incs:?}");
        // ...core's PRIVATE include and spdlog's (private dep) do NOT.
        assert!(!incs.contains(&root.join("src").display().to_string()), "{incs:?}");
        assert!(!incs.contains(&"/store/spdlog/include".to_string()), "{incs:?}");
        // Same for defines.
        assert!(unit.defines.contains(&("CORE_API".to_string(), Some(String::new()))));
        assert!(!unit.defines.iter().any(|(k, _)| k == "CORE_INTERNAL"));
        assert!(!unit.defines.iter().any(|(k, _)| k == "SPDLOG_COMPILED_LIB"));

        // core's OWN units see everything it declared, public and private.
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
        // spdlog's ARCHIVE reaches myapp's link closure (link-only edge)...
        assert!(archives.contains(&"/store/spdlog/libspdlog.a".to_string()), "{archives:?}");
        assert!(archives.contains(&"/store/fmt/libfmt.a".to_string()));
        assert!(archives.contains(&"libcore.a".to_string()));
        // ...even though its compile requirements were stopped (asserted in
        // public_deps_propagate_private_do_not).
        // Order: dependents before dependencies (core before its deps).
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
        // Consumer private include must NOT leak into the interface unit.
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
        // Unique, per-target object path for an out-of-project source.
        let obj = unit.object.display().to_string();
        assert!(obj.starts_with("app.dir/ext/"), "{obj}");
        assert!(obj.ends_with("-extra.cpp.o"), "{obj}");
        // No interface archive in the link line.
        assert!(archive_paths(app).is_empty());
    }

    // ---- sources: globs + extension table ---------------------------------

    #[test]
    fn glob_expansion_sorted_byte_order() {
        let tmp = TempDir::new().unwrap();
        // Created out of order on purpose; expansion must sort.
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
        // Happy path: all four C++ extensions plus C.
        let tmp = TempDir::new().unwrap();
        for f in ["a.cpp", "b.cc", "c.cxx", "d.c++", "e.c"] {
            touch(tmp.path(), f);
        }
        let t = target(TargetKind::Executable, &["a.cpp", "b.cc", "c.cxx", "d.c++", "e.c"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        let langs: Vec<Lang> = find(&plan, "app").units.iter().map(|u| u.lang).collect();
        assert_eq!(langs, vec![Lang::Cxx, Lang::Cxx, Lang::Cxx, Lang::Cxx, Lang::C]);

        // .C is a hard error (case-insensitive filesystem trap).
        // NOTE: name the file distinctly so the case-insensitive host FS
        // can hold it; classification uses the manifest's spelling.
        let err = {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), "upper.C");
            let t = target(TargetKind::Executable, &["upper.C"]);
            let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
            run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string()
        };
        assert!(err.contains(".C"), "{err}");
        assert!(err.contains("rename"), "{err}");

        // Objective-C: clear unsupported error for .m and .mm.
        for objc in ["x.m", "y.mm"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), objc);
            let t = target(TargetKind::Executable, &[objc]);
            let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
            let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
            assert!(err.contains("Objective-C not supported in v0"), "{err}");
        }

        // Unknown extension: hard error, never silently C++.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.rs");
        let t = target(TargetKind::Executable, &["main.rs"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let err = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("unknown extension"), "{err}");
        assert!(err.contains(".rs"), "{err}");
    }

    // ---- link plan ordering -----------------------------------------------

    #[test]
    fn link_order_topological_archives_keep_last() {
        // Diamond: app -> a, b; a -> d; b -> d. d must appear once, AFTER
        // both a and b (keep-LAST).
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
        // Own object first.
        assert!(matches!(&app.link_inputs[0], LinkInput::Object(_)));
        assert_eq!(archive_paths(app), vec!["liba.a", "libb.a", "libd.a"]);
        // Plan order: dependencies first.
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
        assert_eq!(sys, vec!["z", "m"]); // "z" kept at FIRST occurrence
        let fw: Vec<&str> = app
            .link_inputs
            .iter()
            .filter_map(|li| match li {
                LinkInput::Framework(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(fw, vec!["CoreFoundation"]);
        // "z" (from x) must precede y's archive? No — position of first
        // occurrence: x's archive, x's libs, then y's. Verify x's z came
        // before y's archive to prove keep-FIRST (not keep-last).
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

    // ---- link language ----------------------------------------------------

    #[test]
    fn link_language_rules() {
        // Pure C, no deps -> C driver.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.c");
        let t = target(TargetKind::Executable, &["main.c"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::C);

        // Own C++ source -> C++ driver.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "main.cpp");
        let t = target(TargetKind::Executable, &["main.cpp"]);
        let p = project(BTreeMap::new(), BTreeMap::from([("app".to_string(), t)]));
        let plan = run_plan(&p, tmp.path(), &BTreeMap::new()).unwrap();
        assert_eq!(find(&plan, "app").link_lang, Lang::Cxx);

        // C executable with a C++ static lib (even PRIVATE) -> C++ driver.
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

        // C executable with a dependency component -> C++ (conservative).
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

        // Component requires MORE than the target -> component wins.
        let deps = BTreeMap::from([("d".to_string(), dep())]);
        let (mut p, tmp) = exe_project(deps.clone(), &["d::d"]);
        p.targets.get_mut("app").unwrap().cxx_std = Some(17);
        let manifests = BTreeMap::from([("d".to_string(), manifest("d", &[("d::d", c20.clone())]))]);
        let plan = run_plan(&p, tmp.path(), &manifests).unwrap();
        assert_eq!(find(&plan, "app").units[0].std, Some(20));

        // Target requires MORE than the component -> target wins.
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

    // ---- only-filter -------------------------------------------------------

    #[test]
    fn only_filter_selects_transitive_sibling_closure() {
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
        let plan = plan(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            BuildConfig::Release,
            &Profile::default(),
            &["app1".to_string()],
        )
        .unwrap();
        let names: Vec<&str> = plan.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["util", "core1", "app1"]);

        let err = super::plan(
            &p,
            tmp.path(),
            &BTreeMap::new(),
            BuildConfig::Release,
            &Profile::default(),
            &["nosuch".to_string()],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown target `nosuch`"), "{err}");
    }

    // ---- profile flag routing ----------------------------------------------

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
        let plan = plan(&p, tmp.path(), &BTreeMap::new(), BuildConfig::Release, &profile, &[]).unwrap();
        let app = find(&plan, "app");
        let cpp = app.units.iter().find(|u| u.lang == Lang::Cxx).unwrap();
        let c = app.units.iter().find(|u| u.lang == Lang::C).unwrap();
        assert!(cpp.extra_flags.contains(&"-fcxx-only".to_string()));
        assert!(!cpp.extra_flags.contains(&"-fc-only".to_string()));
        assert!(c.extra_flags.contains(&"-fc-only".to_string()));
        assert!(!c.extra_flags.contains(&"-fcxx-only".to_string()));
        assert!(app.link_flags.contains(&"-Wl,-dead_strip".to_string()));
    }

    // ---- cross-package requires (find_dependency chains) -------------------

    #[test]
    fn cross_package_requires_resolve_through_ladder() {
        // spdlog::spdlog requires fmt::fmt, which appears in BOTH manifests;
        // the ladder attributes it to the `fmt` package (depkey prefix).
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
        // fmt's usage requirements reach the consumer through the public
        // `requires` edge...
        assert!(include_dirs(&app.units[0]).contains(&"/store/fmt/include".to_string()));
        // ...and the REAL fmt archive links, after spdlog's.
        assert_eq!(
            archive_paths(app),
            vec!["/store/spdlog/libspdlog.a", "/store/fmt/libfmt.a"]
        );
    }

    #[test]
    fn component_link_requires_is_link_only() {
        // core::core LINK-ONLY-requires impl::impl (a $<LINK_ONLY> edge in
        // its INTERFACE_LINK_LIBRARIES): archive links, includes don't leak.
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
        // Executable output name is the bare target name.
        assert_eq!(find(&plan, "myapp").output, PathBuf::from("myapp"));
        assert_eq!(find(&plan, "myapp").target_deps, vec!["core".to_string()]);
    }
}
