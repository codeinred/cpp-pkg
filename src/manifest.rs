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
        json.into_manifest()
            .with_context(|| format!("decoding manifest {}", path.display()))
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
            c.interface_sources =
                split_cmake_list_local(v).into_iter().map(PathBuf::from).collect();
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

    Ok(manifest)
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
                // Only the outer wrapper is stripped; a genex inside
                // LINK_ONLY may evaluate to anything, so we cannot map it
                // back to an evaluated entry reliably.
                notes.push(format!(
                    "{target}: unhandled generator expression inside LINK_ONLY: '{inner}'"
                ));
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
