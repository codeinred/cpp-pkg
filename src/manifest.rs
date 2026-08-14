//! Package manifests: CPS-style JSON stored beside each artifact entry
//! (CPP_PKG_IMPLEMENTATION.md §2 — CPS with vendor extensions; the on-disk
//! format is CPS-shaped but only the fields we consume are guaranteed).
//!
//! Contract:
//! - Component names are the full exported names ("fmt::fmt").
//! - `from_probe` consumes probe records for ONE config; `location` is keyed
//!   by CMake config name so future configs merge additively.
//! - INTERFACE_LINK_LIBRARIES entries resolve to: another component in this
//!   package -> `requires`; a $<LINK_ONLY:x> entry whose inner value is a
//!   target reference -> `link_requires` (bare libs/paths/frameworks inside
//!   LINK_ONLY classify into their ordinary buckets — those buckets carry no
//!   compile requirements, so they are link-only already);
//!   an absolute path -> `link_paths`; `-lfoo`/plain name -> `system_libs`;
//!   `-framework X` / FRAMEWORK genex or Foo.framework path -> `frameworks`;
//!   a target from ANOTHER package (transitive find_dependency) ->
//!   `requires` with its full name (cross-package refs resolved at graph
//!   time via the naming ladder).
//! - Compile features: only cxx_std_NN is honored (mapped to a std level,
//!   max-merged with cxx-std at graph time); other features are ignored
//!   with a recorded warning (granular features are pre-C++17 legacy).
//! - INTERFACE_SOURCES: paths recorded; consumer compiles them (decided).
//! - Deduplicate targets claimed by multiple probes (transitive
//!   find_dependency): a component whose defining package is ambiguous keeps
//!   ONLY the attribution decided by graph::resolve via exposes-* /
//!   namespace matching — from_probe records everything it saw plus which
//!   find_package call surfaced it (`origin_find_name`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::probe::ProbeRecord;
use crate::schema::BuildConfig;
use crate::Result;
use crate::SCHEMA_VERSION;

/// Version of the extraction pipeline (wave-1 spec Appendix A.8). Bump
/// whenever probe OUTPUT shape changes — store manifest cache paths embed it
/// (store::manifest_path), so warm stores cheaply re-derive their manifests
/// and converge with fresh machines. Artifacts and their config-hash keys are
/// untouched by a bump.
///
/// History: 1 = v0; 2 = wave 1 ($<BOOL:...> evaluation inside LINK_ONLY,
/// non-compilable INTERFACE_SOURCES skipped at extraction).
pub const EXTRACTOR_VERSION: u32 = 2;

/// The builtin pseudo-package spelling manifests carry after the §5.4
/// Threads rewrite. Graph resolves it at naming-ladder step 0; its expansion
/// is a pure function of toolchain identity (toolchain::threads_expansion).
pub const THREADS_BUILTIN_LINK: &str = "builtin:threads";

/// The CMake-shaped spelling upstream packages use (FindThreads).
const THREADS_TARGET: &str = "Threads::Threads";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentKind {
    /// STATIC_LIBRARY -> archive
    Archive,
    /// SHARED_LIBRARY -> dylib (consumable in v0 even though we BUILD static)
    Dylib,
    /// INTERFACE_LIBRARY (header-only)
    Interface,
    /// UNKNOWN imported type: treat location as link input if present
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Component {
    pub kind: Option<ComponentKind>,
    /// CMake config name -> artifact path.
    pub location: BTreeMap<String, PathBuf>,
    pub includes: Vec<PathBuf>,
    pub system_includes: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub compile_options: Vec<String>,
    /// Max cxx_std_NN seen, if any.
    pub cxx_std: Option<u32>,
    pub link_options: Vec<String>,
    /// Full names of required components (compile + link propagation).
    pub requires: Vec<String>,
    /// Link-only requirements ($<LINK_ONLY:...>): artifacts reach the link
    /// closure, compile requirements do not propagate.
    pub link_requires: Vec<String>,
    pub link_paths: Vec<PathBuf>,
    pub system_libs: Vec<String>,
    pub frameworks: Vec<String>,
    pub interface_sources: Vec<PathBuf>,
    /// Which find_package() surfaced this component (attribution input).
    pub origin_find_name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    /// Dependency key this manifest belongs to.
    pub package: String,
    pub components: BTreeMap<String, Component>,
    /// Non-fatal extraction warnings (unhandled genexes, ignored compile
    /// features, unlocated libraries). Persisted so a later `cpp-pkg build`
    /// consuming a cached manifest can still surface them.
    pub notes: Vec<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        let json: ManifestJson = serde_json::from_str(&raw)
            .with_context(|| format!("parsing manifest {}", path.display()))?;
        if json.schema_version != SCHEMA_VERSION {
            bail!(
                "manifest {} has schema-version {}, this cpp-pkg supports {}",
                path.display(),
                json.schema_version,
                SCHEMA_VERSION
            );
        }
        let mut manifest = json
            .into_manifest()
            .with_context(|| format!("decoding manifest {}", path.display()))?;
        // Read-side normalization is normative (spec A.1/A.8): cached
        // manifests written by any extractor version converge with fresh
        // probes without invalidating the artifacts they describe.
        apply_ingestion_transforms(&mut manifest);
        Ok(manifest)
    }

    /// Stable field order / sorted maps for deterministic files.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = ManifestJson::from_manifest(self);
        let mut text = serde_json::to_string_pretty(&json).context("encoding manifest")?;
        text.push('\n');
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, text)
            .with_context(|| format!("writing manifest {}", path.display()))
    }
}

/// Build a manifest from one probe run (one config).
pub fn from_probe(
    dep_key: &str,
    find_name: &str,
    config: BuildConfig,
    records: &[ProbeRecord],
) -> Result<Manifest> {
    // Group records into per-target property maps; a repeated (target,
    // property) record keeps the last value written by the probe.
    let mut by_target: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for r in records {
        by_target
            .entry(r.target.clone())
            .or_default()
            .insert(r.property.clone(), r.value.clone());
    }

    let mut manifest = Manifest {
        package: dep_key.to_string(),
        ..Default::default()
    };

    for (target, props) in &by_target {
        let mut c = Component {
            origin_find_name: find_name.to_string(),
            ..Default::default()
        };

        match prop(props, "TYPE") {
            Some("STATIC_LIBRARY") => c.kind = Some(ComponentKind::Archive),
            Some("SHARED_LIBRARY") => c.kind = Some(ComponentKind::Dylib),
            Some("INTERFACE_LIBRARY") => c.kind = Some(ComponentKind::Interface),
            Some("UNKNOWN_LIBRARY") => c.kind = Some(ComponentKind::Unknown),
            Some(other) => manifest
                .notes
                .push(format!("{target}: unhandled imported target TYPE '{other}'")),
            None => manifest
                .notes
                .push(format!("{target}: probe returned no TYPE")),
        }

        if let Some(loc) = resolve_location(props, config) {
            c.location
                .insert(config_cmake_name(config).to_string(), loc);
        } else if matches!(
            c.kind,
            Some(ComponentKind::Archive) | Some(ComponentKind::Dylib) | Some(ComponentKind::Unknown)
        ) {
            manifest.notes.push(format!(
                "{target}: no IMPORTED_LOCATION resolved for config {}",
                config_cmake_name(config)
            ));
        }

        if let Some(v) = prop(props, "INTERFACE_INCLUDE_DIRECTORIES") {
            c.includes = split_cmake_list_local(v).into_iter().map(PathBuf::from).collect();
        }
        if let Some(v) = prop(props, "INTERFACE_SYSTEM_INCLUDE_DIRECTORIES") {
            c.system_includes = split_cmake_list_local(v).into_iter().map(PathBuf::from).collect();
        }
        if let Some(v) = prop(props, "INTERFACE_COMPILE_DEFINITIONS") {
            for d in split_cmake_list_local(v) {
                match d.split_once('=') {
                    Some((k, val)) => c.defines.push((k.to_string(), Some(val.to_string()))),
                    None => c.defines.push((d, None)),
                }
            }
        }
        if let Some(v) = prop(props, "INTERFACE_COMPILE_OPTIONS") {
            c.compile_options = split_cmake_list_local(v);
        }
        if let Some(v) = prop(props, "INTERFACE_COMPILE_FEATURES") {
            for f in split_cmake_list_local(v) {
                match f.strip_prefix("cxx_std_").and_then(|n| n.parse::<u32>().ok()) {
                    Some(n) => c.cxx_std = Some(c.cxx_std.map_or(n, |cur| cur.max(n))),
                    None => manifest
                        .notes
                        .push(format!("{target}: ignoring compile feature '{f}'")),
                }
            }
        }
        if let Some(v) = prop(props, "INTERFACE_LINK_OPTIONS") {
            for opt in split_cmake_list_local(v) {
                // CMake's SHELL: prefix means "split into words at generate
                // time"; forwarding the literal would hand the linker one
                // mangled argument. A `-framework X` group (the shim's own
                // spelling for frameworks) returns to the frameworks bucket,
                // keeping the extract -> emit -> extract fixpoint closed.
                if let Some(rest) = opt.strip_prefix("SHELL:") {
                    let words: Vec<&str> = rest.split_whitespace().collect();
                    if let ["-framework", name] = words.as_slice() {
                        push_unique(&mut c.frameworks, (*name).to_string());
                    } else {
                        c.link_options.extend(words.iter().map(|w| (*w).to_string()));
                    }
                } else {
                    c.link_options.push(opt);
                }
            }
        }
        if let Some(v) = prop(props, "INTERFACE_SOURCES") {
            // Spec A.3: CMake compiles only sources whose extension it
            // classifies as compilable; headers (and .natvis-style extras)
            // listed in INTERFACE_SOURCES are IDE/install metadata. Recording
            // them would make the consumer's strict source classification
            // reject the whole package (vtz's date patch existed for this).
            // Project sources stay strict — only extracted interfaces skip.
            c.interface_sources = split_cmake_list_local(v)
                .into_iter()
                .filter(|s| is_compilable_source(s))
                .map(PathBuf::from)
                .collect();
        }

        // Link libraries need BOTH forms: the raw property tells us which
        // entries were $<LINK_ONLY:...>-wrapped (evaluation erases the
        // wrapper), the evaluated property gives genex-flattened values for
        // classification.
        let raw = prop(props, "INTERFACE_LINK_LIBRARIES_RAW")
            .map(split_cmake_list_local)
            .unwrap_or_default();
        let evaluated = prop(props, "INTERFACE_LINK_LIBRARIES")
            .map(split_cmake_list_local)
            .unwrap_or_default();
        classify_link_libraries(target, &raw, &evaluated, &mut c, &mut manifest.notes);

        manifest.components.insert(target.clone(), c);
    }

    // Fresh probe output gets the same read-side normalization as cached
    // manifests (spec A.1/A.2/§5.4) so the two can never disagree.
    apply_ingestion_transforms(&mut manifest);
    Ok(manifest)
}

/// CMake's is-compilable classification, restricted to the languages this
/// tool builds. Extensions CMake treats as compilable stay (even ones graph
/// later rejects with its own targeted error, e.g. Objective-C — a loud
/// error beats silently dropping code upstream meant to compile); everything
/// else — headers, .inc/.ipp, .natvis, extension-less — is skipped.
fn is_compilable_source(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    matches!(
        ext,
        Some("c" | "C" | "cc" | "cpp" | "cxx" | "c++" | "m" | "M" | "mm" | "cu")
    )
}

/// Ingestion-time transforms, applied on EVERY manifest read (`load`) and at
/// the end of `from_probe` (wave-1 spec A.1, A.2, §5.4). Idempotent by
/// construction — a transformed manifest passes through unchanged.
pub fn apply_ingestion_transforms(m: &mut Manifest) {
    // A.2: a self-link edge (absl::strings -> absl::strings) is a no-op in
    // CMake's own semantics; dropping it here keeps graph resolution from
    // seeing a cycle of length one.
    for (name, c) in m.components.iter_mut() {
        c.requires.retain(|r| r != name);
        c.link_requires.retain(|r| r != name);
    }

    // A.1: imported targets' interface include dirs are system includes
    // (CMake's own imported-target default): consumers get them as -isystem,
    // after all -I dirs. The moved entries keep their declared order and land
    // ahead of any dirs the package itself marked system, preserving v0's
    // relative search order. graph honors the per-dependency
    // `system-includes = false` opt-out at plan time.
    for c in m.components.values_mut() {
        if !c.includes.is_empty() {
            let mut merged = std::mem::take(&mut c.includes);
            for p in std::mem::take(&mut c.system_includes) {
                if !merged.contains(&p) {
                    merged.push(p);
                }
            }
            c.system_includes = merged;
        }
    }

    // §5.4: Threads::Threads is a builtin pseudo-package. The extracted
    // component (CMake FindThreads' INTERFACE library) is dropped from
    // ownership attribution and references become the symbolic link input
    // `builtin:threads`, whose expansion is a pure function of toolchain
    // identity. This makes store manifests platform-portable: a manifest
    // probed on macOS (where FindThreads records nothing) and one probed on
    // Linux (where it records -pthread) converge.
    let rewrite_refs = match m.components.get(THREADS_TARGET) {
        Some(c) if threads_component_is_builtin_shape(c) => {
            m.components.remove(THREADS_TARGET);
            true
        }
        Some(_) => {
            // An unexpected shape means FindThreads (or an impostor) carried
            // real interface content; rewriting would silently drop it. Keep
            // the literal interface and say so once.
            let note = format!(
                "{THREADS_TARGET}: unexpected extracted shape for the builtin \
                 pseudo-package; keeping the literal interface (expected CMake \
                 FindThreads' INTERFACE library carrying at most -pthread)"
            );
            if !m.notes.contains(&note) {
                m.notes.push(note);
            }
            false
        }
        // No local component: the reference can only mean the builtin
        // (ladder step 0 resolves builtins first and they cannot be
        // shadowed), so rewrite unconditionally.
        None => true,
    };
    if rewrite_refs {
        for c in m.components.values_mut() {
            rewrite_reference(&mut c.requires, THREADS_TARGET, THREADS_BUILTIN_LINK);
            rewrite_reference(&mut c.link_requires, THREADS_TARGET, THREADS_BUILTIN_LINK);
        }
    }
}

/// True iff the component looks like CMake FindThreads' own Threads::Threads:
/// an INTERFACE library whose only possible content is the -pthread flag
/// (spelled as a compile option, link option, or `-lpthread`).
fn threads_component_is_builtin_shape(c: &Component) -> bool {
    matches!(c.kind, Some(ComponentKind::Interface))
        && c.location.is_empty()
        && c.includes.is_empty()
        && c.system_includes.is_empty()
        && c.defines.is_empty()
        && c.cxx_std.is_none()
        && c.requires.is_empty()
        && c.link_requires.is_empty()
        && c.link_paths.is_empty()
        && c.frameworks.is_empty()
        && c.interface_sources.is_empty()
        && c.compile_options.iter().all(|o| o == "-pthread")
        && c.link_options.iter().all(|o| o == "-pthread")
        && c.system_libs.iter().all(|l| l == "pthread")
}

/// Replace `from` with `to` in a push_unique'd reference list, dropping the
/// entry instead if `to` is already present.
fn rewrite_reference(list: &mut Vec<String>, from: &str, to: &str) {
    if let Some(pos) = list.iter().position(|e| e == from) {
        if list.iter().any(|e| e == to) {
            list.remove(pos);
        } else {
            list[pos] = to.to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// Hermeticity scan (wave-1 spec §5.5, layer 1)

/// One undeclared absolute path found by `scan_hermeticity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leak {
    /// Component the path was recorded on.
    pub component: String,
    pub path: PathBuf,
}

/// Path prefixes whose contents are covered by some hash input: store roots
/// (covered by `dep_hashes`) and declared-system-dependency paths (covered
/// by the sysdep hash).
#[derive(Debug, Clone, Default)]
pub struct HermeticityAllow {
    pub store_roots: Vec<PathBuf>,
    pub sysdep_paths: Vec<PathBuf>,
}

/// §5.5 layer 1: police the invariant that every absolute path a manifest
/// records is covered by some hash input. Scans artifact locations, include
/// dirs, link paths, and interface sources; framework references survive
/// extraction as bare names, never paths, so there is nothing to scan there.
/// SDK-rooted paths are deliberately not exempt.
///
/// Runs on probe output and on every cached-manifest read (the caller — cli —
/// wires the allow-list and decides error vs. warning via
/// `--allow-undeclared-system-libs`).
pub fn scan_hermeticity(m: &Manifest, allow: &HermeticityAllow) -> Vec<Leak> {
    let allowed = |p: &Path| {
        allow
            .store_roots
            .iter()
            .chain(allow.sysdep_paths.iter())
            .any(|root| p.starts_with(root))
    };
    let mut leaks = Vec::new();
    for (name, c) in &m.components {
        let paths = c
            .location
            .values()
            .chain(c.includes.iter())
            .chain(c.system_includes.iter())
            .chain(c.link_paths.iter())
            .chain(c.interface_sources.iter());
        for p in paths {
            if !p.is_absolute() || allowed(p) {
                continue;
            }
            let leak = Leak {
                component: name.clone(),
                path: p.clone(),
            };
            if !leaks.contains(&leak) {
                leaks.push(leak);
            }
        }
    }
    leaks
}

/// Look up a property, treating empty and CMake "-NOTFOUND" values as unset.
fn prop<'a>(props: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    match props.get(key).map(String::as_str) {
        Some("") | None => None,
        Some(v) if v.ends_with("-NOTFOUND") => None,
        Some(v) => Some(v),
    }
}

/// CMake spelling of a config; keys `location` and derives the
/// IMPORTED_LOCATION_<CONFIG> suffix (uppercased).
fn config_cmake_name(config: BuildConfig) -> &'static str {
    config.cmake_name()
}

/// CMake's imported-location fallback chain for a requested config.
///
/// A set MAP_IMPORTED_CONFIG_<CONFIG> takes FULL precedence: CMake consults
/// the map before the exact-config location, and a set-but-unsatisfied map
/// means "the project wants no other configuration" — the target reads as
/// not-found rather than falling back (verified against CMake 4.4). Without
/// a map: exact-config location, then the unsuffixed IMPORTED_LOCATION, then
/// the first IMPORTED_CONFIGURATIONS entry that has a location. An empty map
/// entry means the unsuffixed property.
fn resolve_location(props: &BTreeMap<String, String>, config: BuildConfig) -> Option<PathBuf> {
    let suffix = config_cmake_name(config).to_uppercase();

    if let Some(map) = prop(props, &format!("MAP_IMPORTED_CONFIG_{suffix}")) {
        for m in split_cmake_list_keep_empty(map) {
            if m.is_empty() {
                if let Some(v) = prop(props, "IMPORTED_LOCATION") {
                    return Some(PathBuf::from(v));
                }
            } else if let Some(v) =
                prop(props, &format!("IMPORTED_LOCATION_{}", m.to_uppercase()))
            {
                return Some(PathBuf::from(v));
            }
        }
        return None;
    }
    if let Some(v) = prop(props, &format!("IMPORTED_LOCATION_{suffix}")) {
        return Some(PathBuf::from(v));
    }
    if let Some(v) = prop(props, "IMPORTED_LOCATION") {
        return Some(PathBuf::from(v));
    }
    if let Some(configs) = prop(props, "IMPORTED_CONFIGURATIONS") {
        for m in split_cmake_list_local(configs) {
            if let Some(v) = prop(props, &format!("IMPORTED_LOCATION_{}", m.to_uppercase())) {
                return Some(PathBuf::from(v));
            }
        }
    }
    None
}

/// Classify link-library entries into the manifest buckets.
///
/// The raw (unevaluated) list identifies $<LINK_ONLY:x> entries; their inner
/// values are classified immediately and subtracted from the evaluated list
/// (file(GENERATE) evaluates LINK_ONLY to its content, so each clean inner
/// reappears there) so they are not double-classified.
fn classify_link_libraries(
    target: &str,
    raw: &[String],
    evaluated: &[String],
    c: &mut Component,
    notes: &mut Vec<String>,
) {
    // Inner values of LINK_ONLY wrappers still awaiting their evaluated twin.
    let mut link_only_pending: Vec<String> = Vec::new();
    for entry in raw {
        if let Some(inner) = entry
            .strip_prefix("$<LINK_ONLY:")
            .and_then(|rest| rest.strip_suffix('>'))
        {
            if inner.contains("$<") {
                // Spec A.8: $<BOOL:...>-guarded conditionals are evaluated
                // rather than skipped — CMake exports optional system libs
                // this way ($<LINK_ONLY:$<$<BOOL:${LIBRT}>:-lrt>>, abseil),
                // and skipping silently drops the -lrt edge on Linux. The
                // evaluated result re-enters the ordinary classification, so
                // each surviving entry also gets its evaluated twin
                // subtracted below. Any other genex form still can't be
                // mapped back to an evaluated entry reliably, so it keeps
                // the warning.
                match eval_bool_genexes(inner) {
                    Some(evaluated) => {
                        for e in split_cmake_list_local(&evaluated) {
                            classify_link_entry(&e, true, c);
                            link_only_pending.push(e);
                        }
                    }
                    None => notes.push(format!(
                        "{target}: unhandled generator expression inside LINK_ONLY: '{inner}'"
                    )),
                }
            } else if !inner.is_empty() {
                // CMake exports PRIVATE deps of static libraries this way,
                // and the inner value is frequently a bare lib (`m`, `-lz`)
                // or an absolute path, not a target — those must land in
                // their ordinary buckets or graph resolution would treat
                // them as (unknown) component names and fail the plan.
                classify_link_entry(inner, true, c);
                link_only_pending.push(inner.to_string());
            }
        }
    }

    let mut i = 0;
    while i < evaluated.len() {
        let entry = &evaluated[i];
        i += 1;

        if let Some(pos) = link_only_pending.iter().position(|p| p == entry) {
            link_only_pending.remove(pos);
            continue;
        }
        if entry.contains("$<") {
            notes.push(format!(
                "{target}: unhandled generator expression in link libraries: '{entry}'"
            ));
            continue;
        }
        if entry == "-framework" {
            match evaluated.get(i) {
                Some(name) => {
                    push_unique(&mut c.frameworks, name.clone());
                    i += 1;
                }
                None => notes.push(format!(
                    "{target}: trailing '-framework' with no framework name"
                )),
            }
            continue;
        }
        classify_link_entry(entry, false, c);
    }
}

/// Route one flattened link-library entry into its manifest bucket.
/// `link_only` sends target references to `link_requires` instead of
/// `requires`; every other bucket (paths, system libs, frameworks, link
/// options) is shared — none of them carries compile requirements, so they
/// are link-only by construction.
fn classify_link_entry(entry: &str, link_only: bool, c: &mut Component) {
    if entry.contains("::") {
        let bucket = if link_only {
            &mut c.link_requires
        } else {
            &mut c.requires
        };
        push_unique(bucket, entry.to_string());
        return;
    }
    if let Some(name) = entry.strip_prefix("-framework ") {
        push_unique(&mut c.frameworks, name.trim().to_string());
        return;
    }
    if entry.starts_with('/') {
        match framework_name_from_path(entry) {
            Some(name) => push_unique(&mut c.frameworks, name),
            None => {
                let p = PathBuf::from(entry);
                if !c.link_paths.contains(&p) {
                    c.link_paths.push(p);
                }
            }
        }
        return;
    }
    if let Some(lib) = entry.strip_prefix("-l") {
        push_unique(&mut c.system_libs, lib.to_string());
        return;
    }
    if entry.starts_with('-') {
        // Other dash-entries in INTERFACE_LINK_LIBRARIES are link flags per
        // CMake semantics (e.g. -pthread).
        push_unique(&mut c.link_options, entry.to_string());
        return;
    }
    push_unique(&mut c.system_libs, entry.to_string());
}

/// Evaluate a string made of literal text and `$<BOOL:...>`-based generator
/// expressions (spec A.8). Handled forms: `$<BOOL:arg>` (arg taken
/// literally) and the conditional `$<cond:payload>` where `cond` is `0`,
/// `1`, or itself a handled genex, and `payload` may recursively contain
/// handled genexes. Returns None as soon as any other genex form appears —
/// the caller falls back to its unhandled-genex warning rather than guess.
fn eval_bool_genexes(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("$<") {
        out.push_str(&rest[..start]);
        let genex = &rest[start..];
        let end = matching_genex_end(genex)?;
        out.push_str(&eval_one_genex(&genex[2..end - 1])?);
        rest = &genex[end..];
    }
    out.push_str(rest);
    Some(out)
}

/// `s` starts with `$<`; return the index one past its matching `>`,
/// honoring nesting. None on unbalanced input.
fn matching_genex_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'<') {
            depth += 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'>' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

/// Evaluate the content between `$<` and `>`.
fn eval_one_genex(content: &str) -> Option<String> {
    if let Some(arg) = content.strip_prefix("BOOL:") {
        if arg.contains("$<") {
            return None;
        }
        return Some(if cmake_is_true(arg) { "1" } else { "0" }.to_string());
    }
    let (cond_raw, payload) = split_genex_head(content)?;
    let cond = if cond_raw.contains("$<") {
        eval_bool_genexes(cond_raw)?
    } else {
        cond_raw.to_string()
    };
    match cond.as_str() {
        "0" => Some(String::new()),
        "1" => eval_bool_genexes(payload),
        _ => None,
    }
}

/// Split genex content at the first `:` outside any nested `$<...>`.
fn split_genex_head(content: &str) -> Option<(&str, &str)> {
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'$' if bytes.get(i + 1) == Some(&b'<') => {
                depth += 1;
                i += 2;
                continue;
            }
            b'>' if depth > 0 => depth -= 1,
            b':' if depth == 0 => return Some((&content[..i], &content[i + 1..])),
            _ => {}
        }
        i += 1;
    }
    None
}

/// CMake's $<BOOL:...> truth table: false for empty, 0, FALSE, OFF, N, NO,
/// IGNORE, NOTFOUND, and *-NOTFOUND (case-insensitive); true otherwise.
fn cmake_is_true(v: &str) -> bool {
    let upper = v.to_ascii_uppercase();
    !(v.is_empty()
        || matches!(
            upper.as_str(),
            "0" | "FALSE" | "OFF" | "N" | "NO" | "IGNORE" | "NOTFOUND"
        )
        || upper.ends_with("-NOTFOUND"))
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// "/S/L/Frameworks/Cocoa.framework" or ".../Cocoa.framework/Cocoa" ->
/// "Cocoa"; None when no path component ends with ".framework".
fn framework_name_from_path(path: &str) -> Option<String> {
    path.split('/')
        .find_map(|comp| comp.strip_suffix(".framework"))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Split a CMake ;-list honoring `\;` escapes, dropping empty entries.
fn split_cmake_list_local(value: &str) -> Vec<String> {
    crate::probe::split_cmake_list(value)
}

/// Like `split_cmake_list_local` but keeps empty entries — MAP_IMPORTED_CONFIG
/// uses an empty list element to mean "the unsuffixed property".
fn split_cmake_list_keep_empty(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&';') => {
                cur.push(';');
                chars.next();
            }
            ';' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------------------
// On-disk JSON (CPS-shaped). Struct field order is the file field order;
// maps are BTreeMaps, so serialization is deterministic. Vendor-specific
// fields that CPS has no home for keep plain names — only the fields we
// consume are guaranteed.

#[derive(Serialize, Deserialize)]
struct ManifestJson {
    schema_version: u32,
    name: String,
    components: BTreeMap<String, ComponentJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct ComponentJson {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    location: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    system_includes: Vec<String>,
    /// "KEY=VALUE" or bare "KEY" strings (CPS-style define spelling).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    defines: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    compile_options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cxx_std: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    link_options: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    link_requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    link_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    system_libs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    frameworks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    interface_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    origin_find_name: String,
}

fn kind_to_str(kind: &ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Archive => "archive",
        ComponentKind::Dylib => "dylib",
        ComponentKind::Interface => "interface",
        ComponentKind::Unknown => "unknown",
    }
}

fn kind_from_str(s: &str) -> Result<ComponentKind> {
    Ok(match s {
        "archive" => ComponentKind::Archive,
        "dylib" => ComponentKind::Dylib,
        "interface" => ComponentKind::Interface,
        "unknown" => ComponentKind::Unknown,
        other => bail!("unknown component type '{other}'"),
    })
}

fn path_to_string(p: &Path) -> String {
    // Store paths are cpp-pkg-created and UTF-8 in practice; lossy keeps
    // save infallible rather than plumbing a rare error nobody can act on.
    p.to_string_lossy().into_owned()
}

impl ManifestJson {
    fn from_manifest(m: &Manifest) -> ManifestJson {
        ManifestJson {
            schema_version: SCHEMA_VERSION,
            name: m.package.clone(),
            components: m
                .components
                .iter()
                .map(|(name, c)| (name.clone(), ComponentJson::from_component(c)))
                .collect(),
            notes: m.notes.clone(),
        }
    }

    fn into_manifest(self) -> Result<Manifest> {
        let mut components = BTreeMap::new();
        for (name, cj) in self.components {
            components.insert(name, cj.into_component()?);
        }
        Ok(Manifest {
            package: self.name,
            components,
            notes: self.notes,
        })
    }
}

impl ComponentJson {
    fn from_component(c: &Component) -> ComponentJson {
        ComponentJson {
            kind: c.kind.as_ref().map(|k| kind_to_str(k).to_string()),
            location: c
                .location
                .iter()
                .map(|(cfg, p)| (cfg.clone(), path_to_string(p)))
                .collect(),
            includes: c.includes.iter().map(|p| path_to_string(p)).collect(),
            system_includes: c.system_includes.iter().map(|p| path_to_string(p)).collect(),
            defines: c
                .defines
                .iter()
                .map(|(k, v)| match v {
                    Some(v) => format!("{k}={v}"),
                    None => k.clone(),
                })
                .collect(),
            compile_options: c.compile_options.clone(),
            cxx_std: c.cxx_std,
            link_options: c.link_options.clone(),
            requires: c.requires.clone(),
            link_requires: c.link_requires.clone(),
            link_paths: c.link_paths.iter().map(|p| path_to_string(p)).collect(),
            system_libs: c.system_libs.clone(),
            frameworks: c.frameworks.clone(),
            interface_sources: c.interface_sources.iter().map(|p| path_to_string(p)).collect(),
            origin_find_name: c.origin_find_name.clone(),
        }
    }

    fn into_component(self) -> Result<Component> {
        Ok(Component {
            kind: self.kind.as_deref().map(kind_from_str).transpose()?,
            location: self
                .location
                .into_iter()
                .map(|(cfg, p)| (cfg, PathBuf::from(p)))
                .collect(),
            includes: self.includes.into_iter().map(PathBuf::from).collect(),
            system_includes: self.system_includes.into_iter().map(PathBuf::from).collect(),
            defines: self
                .defines
                .into_iter()
                .map(|d| match d.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (d, None),
                })
                .collect(),
            compile_options: self.compile_options,
            cxx_std: self.cxx_std,
            link_options: self.link_options,
            requires: self.requires,
            link_requires: self.link_requires,
            link_paths: self.link_paths.into_iter().map(PathBuf::from).collect(),
            system_libs: self.system_libs,
            frameworks: self.frameworks,
            interface_sources: self.interface_sources.into_iter().map(PathBuf::from).collect(),
            origin_find_name: self.origin_find_name,
        })
    }
}


// Tests for private helpers live here; the public-API suite is in
// tests/manifest_test.rs.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_list_split_escapes() {
        assert_eq!(
            split_cmake_list_local(r"a\;b;c;;d"),
            vec!["a;b".to_string(), "c".to_string(), "d".to_string()]
        );
        assert_eq!(split_cmake_list_local(""), Vec::<String>::new());
        assert_eq!(
            split_cmake_list_keep_empty(";Debug"),
            vec![String::new(), "Debug".to_string()]
        );
    }

    #[test]
    fn manifest_framework_name_extraction() {
        assert_eq!(
            framework_name_from_path("/System/Library/Frameworks/Cocoa.framework"),
            Some("Cocoa".to_string())
        );
        assert_eq!(
            framework_name_from_path("/S/L/F/Metal.framework/Metal"),
            Some("Metal".to_string())
        );
        assert_eq!(framework_name_from_path("/usr/lib/libz.a"), None);
        assert_eq!(framework_name_from_path("/x/.framework"), None);
    }

    #[test]
    fn manifest_map_imported_config_wins_over_exact_location() {
        // CMake gives a set MAP full precedence over IMPORTED_LOCATION_<CFG>
        // (verified against CMake 4.4): Debug mapped to Release must resolve
        // the Release artifact even though a Debug location exists.
        let props = BTreeMap::from([
            ("MAP_IMPORTED_CONFIG_DEBUG".to_string(), "Release".to_string()),
            ("IMPORTED_LOCATION_DEBUG".to_string(), "/s/libdbg.a".to_string()),
            ("IMPORTED_LOCATION_RELEASE".to_string(), "/s/librel.a".to_string()),
        ]);
        assert_eq!(
            resolve_location(&props, BuildConfig::Debug),
            Some(PathBuf::from("/s/librel.a"))
        );
    }

    #[test]
    fn manifest_map_imported_config_unsatisfied_is_not_found() {
        // A set-but-unsatisfied map means "the project wants no other
        // configuration": no fallback to exact/unsuffixed/any-config.
        let props = BTreeMap::from([
            ("MAP_IMPORTED_CONFIG_DEBUG".to_string(), "MinSizeRel".to_string()),
            ("IMPORTED_LOCATION_DEBUG".to_string(), "/s/libdbg.a".to_string()),
            ("IMPORTED_LOCATION".to_string(), "/s/libplain.a".to_string()),
            ("IMPORTED_CONFIGURATIONS".to_string(), "Debug".to_string()),
        ]);
        assert_eq!(resolve_location(&props, BuildConfig::Debug), None);
        // The same properties without the map fall back normally.
        let mut without = props.clone();
        without.remove("MAP_IMPORTED_CONFIG_DEBUG");
        assert_eq!(
            resolve_location(&without, BuildConfig::Debug),
            Some(PathBuf::from("/s/libdbg.a"))
        );
    }

    #[test]
    fn manifest_link_only_bare_entries_classify_into_buckets() {
        // The LINK_ONLY wrapper around bare libs / paths / flags must not
        // produce bogus `link_requires` component references (they would
        // fail graph resolution); only target references stay link-only.
        let raw: Vec<String> = [
            "$<LINK_ONLY:m>",
            "$<LINK_ONLY:-lpthread>",
            "$<LINK_ONLY:/opt/x/libpng.a>",
            "$<LINK_ONLY:zlib::zlib>",
            "$<LINK_ONLY:-Wl,-undefined,error>",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let evaluated: Vec<String> =
            ["m", "-lpthread", "/opt/x/libpng.a", "zlib::zlib", "-Wl,-undefined,error"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let mut c = Component::default();
        let mut notes = Vec::new();
        classify_link_libraries("t", &raw, &evaluated, &mut c, &mut notes);
        assert_eq!(c.link_requires, vec!["zlib::zlib"]);
        assert_eq!(c.system_libs, vec!["m", "pthread"]);
        assert_eq!(c.link_paths, vec![PathBuf::from("/opt/x/libpng.a")]);
        assert_eq!(c.link_options, vec!["-Wl,-undefined,error"]);
        assert!(c.requires.is_empty());
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn manifest_eval_bool_genexes() {
        // The abseil corpus shapes: value-carrying and empty BOOL guards.
        assert_eq!(
            eval_bool_genexes("$<$<BOOL:/usr/lib/librt.a>:-lrt>").as_deref(),
            Some("-lrt")
        );
        assert_eq!(eval_bool_genexes("$<$<BOOL:>:-lrt>").as_deref(), Some(""));
        assert_eq!(
            eval_bool_genexes("$<$<BOOL:LIBRT-NOTFOUND>:-lrt>").as_deref(),
            Some("")
        );
        // Bare BOOL, literal prefix/suffix, list payloads, nested payloads.
        assert_eq!(eval_bool_genexes("$<BOOL:ON>").as_deref(), Some("1"));
        assert_eq!(eval_bool_genexes("a$<$<BOOL:x>:b>c").as_deref(), Some("abc"));
        assert_eq!(eval_bool_genexes("$<$<BOOL:1>:a;b>").as_deref(), Some("a;b"));
        assert_eq!(
            eval_bool_genexes("$<$<BOOL:1>:$<$<BOOL:>:x>y>").as_deref(),
            Some("y")
        );
        assert_eq!(eval_bool_genexes("$<1:z>").as_deref(), Some("z"));
        assert_eq!(eval_bool_genexes("$<0:z>").as_deref(), Some(""));
        // Anything else fails closed.
        assert_eq!(eval_bool_genexes("$<$<CONFIG:Debug>:dbg>"), None);
        assert_eq!(eval_bool_genexes("$<TARGET_OBJECTS:t>"), None);
        assert_eq!(eval_bool_genexes("$<BOOL:$<CONFIG>>"), None);
        assert_eq!(eval_bool_genexes("$<unclosed"), None);
    }

    #[test]
    fn manifest_link_only_bool_genex_evaluates() {
        // True guard: -lrt lands in system_libs (link-only bucket), its
        // evaluated twin is subtracted, and no warning is recorded.
        let raw = vec!["$<LINK_ONLY:$<$<BOOL:/usr/lib/librt.a>:-lrt>>".to_string()];
        let evaluated = vec!["-lrt".to_string()];
        let mut c = Component::default();
        let mut notes = Vec::new();
        classify_link_libraries("t", &raw, &evaluated, &mut c, &mut notes);
        assert_eq!(c.system_libs, vec!["rt"]);
        assert!(c.link_options.is_empty());
        assert!(notes.is_empty(), "{notes:?}");

        // False guard: nothing recorded, nothing warned.
        let raw = vec!["$<LINK_ONLY:$<$<BOOL:>:-lrt>>".to_string()];
        let mut c = Component::default();
        let mut notes = Vec::new();
        classify_link_libraries("t", &raw, &[], &mut c, &mut notes);
        assert!(c.system_libs.is_empty());
        assert!(notes.is_empty(), "{notes:?}");
    }

    fn interface_component() -> Component {
        Component {
            kind: Some(ComponentKind::Interface),
            ..Default::default()
        }
    }

    #[test]
    fn manifest_transforms_move_includes_to_system() {
        let mut c = interface_component();
        c.includes = vec![PathBuf::from("/s/a"), PathBuf::from("/s/b")];
        c.system_includes = vec![PathBuf::from("/s/sys"), PathBuf::from("/s/a")];
        let mut m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([("d::d".to_string(), c)]),
            notes: vec![],
        };
        apply_ingestion_transforms(&mut m);
        let c = &m.components["d::d"];
        assert!(c.includes.is_empty());
        // Declared order first, pre-marked system entries after, deduped.
        assert_eq!(
            c.system_includes,
            vec![PathBuf::from("/s/a"), PathBuf::from("/s/b"), PathBuf::from("/s/sys")]
        );
        // Idempotent.
        let snapshot = m.clone();
        apply_ingestion_transforms(&mut m);
        assert_eq!(m, snapshot);
    }

    #[test]
    fn manifest_transforms_drop_self_edges() {
        let mut c = interface_component();
        c.requires = vec!["d::d".to_string(), "d::other".to_string()];
        c.link_requires = vec!["d::d".to_string()];
        let mut m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([("d::d".to_string(), c)]),
            notes: vec![],
        };
        apply_ingestion_transforms(&mut m);
        assert_eq!(m.components["d::d"].requires, vec!["d::other"]);
        assert!(m.components["d::d"].link_requires.is_empty());
    }

    #[test]
    fn manifest_transforms_rewrite_threads_builtin() {
        let mut threads = interface_component();
        threads.compile_options = vec!["-pthread".to_string()];
        threads.link_options = vec!["-pthread".to_string()];
        let mut user = interface_component();
        user.requires = vec!["Threads::Threads".to_string()];
        user.link_requires = vec!["Threads::Threads".to_string()];
        let mut m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([
                ("Threads::Threads".to_string(), threads),
                ("d::d".to_string(), user),
            ]),
            notes: vec![],
        };
        apply_ingestion_transforms(&mut m);
        assert!(!m.components.contains_key("Threads::Threads"));
        assert_eq!(m.components["d::d"].requires, vec![THREADS_BUILTIN_LINK]);
        assert_eq!(m.components["d::d"].link_requires, vec![THREADS_BUILTIN_LINK]);
        assert!(m.notes.is_empty(), "{:?}", m.notes);
        // Idempotent.
        let snapshot = m.clone();
        apply_ingestion_transforms(&mut m);
        assert_eq!(m, snapshot);
    }

    #[test]
    fn manifest_transforms_rewrite_threads_reference_without_component() {
        // Transitive reference to a Threads::Threads owned by another
        // package's probe: still the builtin (it cannot be shadowed).
        let mut user = interface_component();
        user.requires = vec!["Threads::Threads".to_string(), THREADS_BUILTIN_LINK.to_string()];
        let mut m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([("d::d".to_string(), user)]),
            notes: vec![],
        };
        apply_ingestion_transforms(&mut m);
        // The literal spelling collapses into the already-present builtin.
        assert_eq!(m.components["d::d"].requires, vec![THREADS_BUILTIN_LINK]);
    }

    #[test]
    fn manifest_transforms_keep_unexpected_threads_shape() {
        let mut threads = interface_component();
        threads.defines = vec![("SURPRISE".to_string(), None)];
        let mut user = interface_component();
        user.requires = vec!["Threads::Threads".to_string()];
        let mut m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([
                ("Threads::Threads".to_string(), threads),
                ("d::d".to_string(), user),
            ]),
            notes: vec![],
        };
        apply_ingestion_transforms(&mut m);
        // Literal interface kept, references untouched, one warning note.
        assert!(m.components.contains_key("Threads::Threads"));
        assert_eq!(m.components["d::d"].requires, vec!["Threads::Threads"]);
        assert_eq!(m.notes.len(), 1);
        assert!(m.notes[0].contains("unexpected extracted shape"));
        // Idempotent — the note is not duplicated on a second read.
        apply_ingestion_transforms(&mut m);
        assert_eq!(m.notes.len(), 1);
    }

    #[test]
    fn manifest_scan_hermeticity_flags_undeclared_absolute_paths() {
        let mut c = interface_component();
        c.system_includes = vec![
            PathBuf::from("/store/pkg/zstd-abc/install/include"),
            PathBuf::from("/opt/homebrew/include"),
        ];
        c.link_paths = vec![PathBuf::from("/opt/homebrew/lib/libzstd.dylib")];
        c.interface_sources = vec![PathBuf::from("relative/extra.cpp")];
        let m = Manifest {
            package: "dep".to_string(),
            components: BTreeMap::from([("z::z".to_string(), c)]),
            notes: vec![],
        };
        let allow = HermeticityAllow {
            store_roots: vec![PathBuf::from("/store")],
            sysdep_paths: vec![],
        };
        let leaks = scan_hermeticity(&m, &allow);
        assert_eq!(
            leaks.iter().map(|l| l.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/opt/homebrew/include"),
                PathBuf::from("/opt/homebrew/lib/libzstd.dylib"),
            ]
        );
        assert!(leaks.iter().all(|l| l.component == "z::z"));

        // A declared sysdep allowlists its paths.
        let allow = HermeticityAllow {
            store_roots: vec![PathBuf::from("/store")],
            sysdep_paths: vec![PathBuf::from("/opt/homebrew")],
        };
        assert!(scan_hermeticity(&m, &allow).is_empty());
    }

    #[test]
    fn manifest_prop_treats_notfound_as_unset() {
        let props = BTreeMap::from([
            ("A".to_string(), "".to_string()),
            ("B".to_string(), "B-NOTFOUND".to_string()),
            ("C".to_string(), "value".to_string()),
        ]);
        assert_eq!(prop(&props, "A"), None);
        assert_eq!(prop(&props, "B"), None);
        assert_eq!(prop(&props, "C"), Some("value"));
        assert_eq!(prop(&props, "missing"), None);
    }
}
