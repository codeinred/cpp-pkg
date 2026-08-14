//! Tier-2 extraction: probe an INSTALLED config-file package for its imported
//! targets and their usage requirements (CPP_PKG_IMPLEMENTATION.md §9).
//!
//! Mechanism (established in design):
//! - Generate a throwaway CMake project (in a temp dir under the artifact
//!   entry) that: snapshots the IMPORTED_TARGETS directory property, calls
//!   find_package(<find_name> REQUIRED CONFIG) with CMAKE_PREFIX_PATH set to
//!   the needs closure + this package's install dir, diffs the property to
//!   get the new imported targets, and for each target emits one
//!   file(GENERATE) whose CONTENT is built from $<TARGET_PROPERTY:...>
//!   generator expressions (file(GENERATE) evaluates genexes; use its TARGET
//!   argument for target-dependent expressions). Configured with the same
//!   toolchain file + CMAKE_BUILD_TYPE as the dependency build so per-config
//!   genexes flatten identically.
//! - WIRE FORMAT (decided): record-oriented text, NOT JSON — CMake cannot
//!   safely JSON-escape arbitrary property values. One record per
//!   (target, property): fields separated by \x1F (unit separator), records
//!   by \x1E (record separator):  target \x1F property \x1F value
//!   CMake ;-lists arrive as the raw value; splitting on unescaped ';' is
//!   done HERE in Rust (handle \; escapes).
//! - Properties probed per target (v0 frozen list):
//!   TYPE, IMPORTED_LOCATION_<CONFIG> (with fallbacks: IMPORTED_LOCATION,
//!   IMPORTED_LOCATION_RELEASE ... per CMake's config fallback rules),
//!   IMPORTED_IMPLIB is N/A on macOS, INTERFACE_INCLUDE_DIRECTORIES,
//!   INTERFACE_SYSTEM_INCLUDE_DIRECTORIES, INTERFACE_COMPILE_DEFINITIONS,
//!   INTERFACE_COMPILE_OPTIONS, INTERFACE_COMPILE_FEATURES,
//!   INTERFACE_LINK_LIBRARIES, INTERFACE_LINK_OPTIONS, INTERFACE_SOURCES,
//!   IMPORTED_LINK_INTERFACE_LANGUAGES.
//! - $<LINK_ONLY:...> in INTERFACE_LINK_LIBRARIES must SURVIVE to the
//!   records (probe emits both a genex-evaluated value and the raw property
//!   value for LINK_LIBRARIES so manifest.rs can distinguish link-only
//!   entries: property INTERFACE_LINK_LIBRARIES_RAW carries the unevaluated
//!   string).
//!   Verified against CMake 4.4: $<TARGET_PROPERTY:t,INTERFACE_LINK_LIBRARIES>
//!   returns the property content UNevaluated (link libraries are
//!   special-cased), so the evaluated record is produced with
//!   $<TARGET_GENEX_EVAL:...>, which flattens config genexes and collapses
//!   $<LINK_ONLY:x> to x (its content — NOT empty). The evaluated record
//!   therefore lists every link entry flattened; whether an entry is
//!   link-only comes from the RAW record.
//! - Failure modes: find_package failing in the probe is a bug in our prefix
//!   path assembly or the package -> surface the CMake log path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};

use crate::schema::BuildConfig;
use crate::toolchain::Toolchain;
use crate::Result;

/// Property name for the raw (unevaluated) INTERFACE_LINK_LIBRARIES record;
/// manifest.rs matches on this to recover $<LINK_ONLY:...> edges.
pub const RAW_LINK_LIBRARIES_PROP: &str = "INTERFACE_LINK_LIBRARIES_RAW";

const RS: char = '\u{1E}'; // record separator
const US: char = '\u{1F}'; // field separator
const GS: char = '\u{1D}'; // transport-escape introducer for the raw record

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRecord {
    pub target: String,
    pub property: String,
    /// Raw single value as written by CMake (';'-splitting done by caller
    /// via `split_cmake_list`).
    pub value: String,
}

/// Run the probe. `find_name` is DependencySpec.find_package or the dep key.
/// `prefix_path` = needs closure installs + this package's install dir.
pub fn probe_installed(
    find_name: &str,
    prefix_path: &[std::path::PathBuf],
    config: BuildConfig,
    toolchain: &Toolchain,
    work_dir: &Path,
) -> Result<Vec<ProbeRecord>> {
    validate_find_name(find_name)?;
    fs::create_dir_all(work_dir)
        .with_context(|| format!("creating probe work dir {}", work_dir.display()))?;

    // A stale build tree could carry probe-out files from a previous probe
    // with a different target set; start from a clean build dir every time
    // (probe configures are cheap).
    let build_dir = work_dir.join("build");
    let _ = fs::remove_dir_all(&build_dir);
    fs::create_dir_all(&build_dir)
        .with_context(|| format!("creating probe build dir {}", build_dir.display()))?;

    let toolchain_file = write_probe_toolchain(work_dir, toolchain)?;
    fs::write(
        work_dir.join("CMakeLists.txt"),
        render_probe_cmakelists(find_name, config),
    )
    .with_context(|| format!("writing probe CMakeLists.txt in {}", work_dir.display()))?;

    // CMake's list separator, regardless of the host PATH separator.
    let prefix = prefix_path
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(";");

    let mut cmd = Command::new("cmake");
    cmd.arg("-G")
        .arg("Ninja")
        .arg("-S")
        .arg(work_dir)
        .arg("-B")
        .arg(&build_dir)
        .arg(format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            toolchain_file.to_string_lossy()
        ))
        .arg(format!("-DCMAKE_BUILD_TYPE={}", cmake_config_name(config)))
        // The probe project itself declares 3.24, but the package's config
        // scripts may call cmake_minimum_required with pre-3.5 versions that
        // CMake 4.x rejects outright.
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5");
    // Same find-control ownership as the dependency build (§5), so the
    // probe's find_package cannot resolve through a registry/env backdoor
    // the build configure had closed.
    cmd.args(crate::cmake_build::find_control_args());
    if !prefix.is_empty() {
        cmd.arg(format!("-DCMAKE_PREFIX_PATH={prefix}"));
    }
    apply_scrubbed_env(&mut cmd);

    let output = cmd
        .output()
        .context("failed to run `cmake` (is it on PATH?)")?;
    if !output.status.success() {
        bail!(
            "probe configure for find_package({find_name}) failed ({status})\n\
             work dir: {work}\n\
             CMake logs: {logs}\n\
             --- stdout (tail) ---\n{out}\n--- stderr (tail) ---\n{err}",
            status = output.status,
            work = work_dir.display(),
            logs = build_dir.join("CMakeFiles").display(),
            out = tail_str(&output.stdout, 4000),
            err = tail_str(&output.stderr, 4000),
        );
    }

    // The probed package's own find_dependency calls must have landed in the
    // store prefixes, exactly like the build configure's (§5).
    let mut allowed: Vec<PathBuf> = prefix_path.to_vec();
    allowed.push(work_dir.to_path_buf());
    crate::cmake_build::check_find_package_leaks(
        find_name,
        &build_dir.join("CMakeCache.txt"),
        &allowed,
    )?;

    // file(GENERATE) runs during the generate step, which `cmake -S -B`
    // completes for single-config Ninja — no --build needed.
    let raw = read_probe_outputs(&build_dir.join("probe-out"))?;
    let mut records = parse_records(&raw)?;
    for rec in &mut records {
        if rec.property == RAW_LINK_LIBRARIES_PROP {
            rec.value = decode_raw_transport(&rec.value);
        }
    }
    Ok(records)
}

/// Split a CMake ;-list, honoring `\;` escapes.
///
/// Empty elements are dropped, matching CMake's own list expansion (and the
/// `;;` holes that genex evaluation of `$<LINK_ONLY:...>` entries leaves
/// behind).
pub fn split_cmake_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&';') {
            current.push(';');
            chars.next();
        } else if c == ';' {
            if !current.is_empty() {
                items.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        items.push(current);
    }
    items
}

/// Parse the probe output file (\x1E records, \x1F fields).
pub fn parse_records(raw: &str) -> Result<Vec<ProbeRecord>> {
    let mut records = Vec::new();
    for chunk in raw.split(RS) {
        // Every record is RS-terminated, so the final split element is empty;
        // tolerate stray whitespace-only chunks from editors/concatenation.
        if chunk.trim().is_empty() {
            continue;
        }
        let mut fields = chunk.splitn(3, US);
        let (target, property, value) = match (fields.next(), fields.next(), fields.next()) {
            (Some(t), Some(p), Some(v)) => (t, p, v),
            _ => bail!(
                "malformed probe record (expected target\\x1Fproperty\\x1Fvalue): {:?}",
                truncate_for_error(chunk)
            ),
        };
        if target.is_empty() || property.is_empty() {
            bail!(
                "malformed probe record (empty target or property): {:?}",
                truncate_for_error(chunk)
            );
        }
        records.push(ProbeRecord {
            target: target.to_string(),
            property: property.to_string(),
            // Empty values are legitimate: genexes for missing properties
            // evaluate to "" and the record is emitted anyway.
            value: value.to_string(),
        });
    }
    Ok(records)
}

// ---------------------------------------------------------------------------
// Probe project generation
// ---------------------------------------------------------------------------

/// CMake spelling of the config (shortcut for call sites in this module).
fn cmake_config_name(config: BuildConfig) -> &'static str {
    config.cmake_name()
}

fn config_upper(config: BuildConfig) -> &'static str {
    match config {
        BuildConfig::Debug => "DEBUG",
        BuildConfig::Release => "RELEASE",
        BuildConfig::RelWithDebInfo => "RELWITHDEBINFO",
        BuildConfig::MinSizeRel => "MINSIZEREL",
    }
}

/// The frozen v0 property list, specialized for the active config. The
/// active config's IMPORTED_LOCATION_<CONFIG> comes first; the other
/// per-config locations (plus NOCONFIG, which exports from build-type-less
/// packages use) are emitted too so manifest.rs can apply CMake's imported-
/// configuration fallback rules without a second probe.
fn probed_properties(config: BuildConfig) -> Vec<String> {
    let active = config_upper(config);
    let mut props = vec![
        "TYPE".to_string(),
        "IMPORTED_LOCATION".to_string(),
        format!("IMPORTED_LOCATION_{active}"),
    ];
    for c in ["DEBUG", "RELEASE", "RELWITHDEBINFO", "MINSIZEREL", "NOCONFIG"] {
        if c != active {
            props.push(format!("IMPORTED_LOCATION_{c}"));
        }
    }
    props.push("IMPORTED_CONFIGURATIONS".to_string());
    props.push(format!("MAP_IMPORTED_CONFIG_{active}"));
    for p in [
        "INTERFACE_INCLUDE_DIRECTORIES",
        "INTERFACE_SYSTEM_INCLUDE_DIRECTORIES",
        "INTERFACE_COMPILE_DEFINITIONS",
        "INTERFACE_COMPILE_OPTIONS",
        "INTERFACE_COMPILE_FEATURES",
        "INTERFACE_LINK_LIBRARIES",
        "INTERFACE_LINK_OPTIONS",
        "INTERFACE_SOURCES",
        "IMPORTED_LINK_INTERFACE_LANGUAGES",
    ] {
        props.push(p.to_string());
    }
    props
}

/// The probe CMakeLists. Placeholders are substituted textually (a template
/// avoids format!-brace noise around CMake's ${...} syntax).
///
/// The raw INTERFACE_LINK_LIBRARIES value is read at configure time with
/// get_target_property and spliced into the file(GENERATE) content. Because
/// file(GENERATE) evaluates generator expressions in its CONTENT, any `$` in
/// the raw value would be re-evaluated — defeating the whole point of the raw
/// record — so the value is transport-escaped first: GS (\x1D) introduces a
/// two-char escape (G=GS, R=RS, U=US, D=$), decoded in decode_raw_transport.
/// GS is escaped before the others so the escape sequences themselves are
/// never re-escaped.
const PROBE_TEMPLATE: &str = r#"cmake_minimum_required(VERSION 3.24)

set(CMAKE_BUILD_TYPE "@CONFIG@")

# CXX, not NONE: package config files routinely inspect compiler/language
# variables and misbehave without an enabled language.
project(cppkg_probe LANGUAGES CXX)

string(ASCII 29 GS)
string(ASCII 30 RS)
string(ASCII 31 US)

get_property(_cppkg_pre DIRECTORY PROPERTY IMPORTED_TARGETS)
find_package(@FIND_NAME@ REQUIRED CONFIG)
get_property(_cppkg_post DIRECTORY PROPERTY IMPORTED_TARGETS)

set(_cppkg_new "")
foreach(_cppkg_t IN LISTS _cppkg_post)
  if(NOT _cppkg_t IN_LIST _cppkg_pre)
    list(APPEND _cppkg_new "${_cppkg_t}")
  endif()
endforeach()

set(_cppkg_props
@PROPS@
)

set(_cppkg_i 0)
foreach(_cppkg_t IN LISTS _cppkg_new)
  set(_cppkg_c "")
  foreach(_cppkg_p IN LISTS _cppkg_props)
    if(_cppkg_p STREQUAL "INTERFACE_LINK_LIBRARIES")
      # TARGET_PROPERTY does NOT re-evaluate this property's content (CMake
      # special-cases link libraries), so config genexes would survive
      # verbatim; TARGET_GENEX_EVAL forces the flattening. $<LINK_ONLY:x>
      # collapses to x here — the RAW record below tells them apart.
      string(APPEND _cppkg_c "${_cppkg_t}${US}${_cppkg_p}${US}$<TARGET_GENEX_EVAL:${_cppkg_t},$<TARGET_PROPERTY:${_cppkg_t},${_cppkg_p}>>${RS}")
    else()
      string(APPEND _cppkg_c "${_cppkg_t}${US}${_cppkg_p}${US}$<TARGET_PROPERTY:${_cppkg_t},${_cppkg_p}>${RS}")
    endif()
  endforeach()

  get_target_property(_cppkg_raw "${_cppkg_t}" INTERFACE_LINK_LIBRARIES)
  if(_cppkg_raw STREQUAL "_cppkg_raw-NOTFOUND")
    set(_cppkg_raw "")
  endif()
  string(REPLACE "${GS}" "${GS}G" _cppkg_raw "${_cppkg_raw}")
  string(REPLACE "${RS}" "${GS}R" _cppkg_raw "${_cppkg_raw}")
  string(REPLACE "${US}" "${GS}U" _cppkg_raw "${_cppkg_raw}")
  string(REPLACE "$" "${GS}D" _cppkg_raw "${_cppkg_raw}")
  string(APPEND _cppkg_c "${_cppkg_t}${US}INTERFACE_LINK_LIBRARIES_RAW${US}${_cppkg_raw}${RS}")

  file(GENERATE OUTPUT "probe-out/${_cppkg_i}.rec" CONTENT "${_cppkg_c}" TARGET "${_cppkg_t}")
  math(EXPR _cppkg_i "${_cppkg_i} + 1")
endforeach()
"#;

fn render_probe_cmakelists(find_name: &str, config: BuildConfig) -> String {
    let props = probed_properties(config)
        .iter()
        .map(|p| format!("  {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    PROBE_TEMPLATE
        .replace("@CONFIG@", cmake_config_name(config))
        .replace("@FIND_NAME@", find_name)
        .replace("@PROPS@", &props)
}

/// find_name is interpolated into CMake source; restrict it to characters
/// that cannot alter parsing (package names in the wild are [A-Za-z0-9_.-]).
fn validate_find_name(find_name: &str) -> Result<()> {
    let ok = !find_name.is_empty()
        && find_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        bail!("invalid find_package name for probe: {find_name:?} (allowed: [A-Za-z0-9_.-]+)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Toolchain file + process environment
// ---------------------------------------------------------------------------

/// Minimal toolchain file pinning the detected compilers, mirroring what the
/// dependency build uses so per-config/per-compiler genexes flatten the same
/// way. (cmake_build::write_toolchain_file is the eventual single source of
/// truth; the probe keeps a local writer so it stays independently testable.)
fn write_probe_toolchain(dir: &Path, toolchain: &Toolchain) -> Result<PathBuf> {
    let mut content = String::from("# Generated by cpp-pkg probe. Compilers are pinned;\n# the environment is never consulted.\n");
    content.push_str(&format!(
        "set(CMAKE_C_COMPILER \"{}\")\n",
        cmake_quote(&toolchain.cc.to_string_lossy())
    ));
    content.push_str(&format!(
        "set(CMAKE_CXX_COMPILER \"{}\")\n",
        cmake_quote(&toolchain.cxx.to_string_lossy())
    ));
    // CMAKE_AR must be a cache entry to survive into the generated build
    // system; a normal toolchain-file set() is discarded.
    content.push_str(&format!(
        "set(CMAKE_AR \"{}\" CACHE FILEPATH \"archiver\")\n",
        cmake_quote(&toolchain.ar.to_string_lossy())
    ));
    if let Some(sdk) = &toolchain.sdk_path {
        content.push_str(&format!(
            "set(CMAKE_OSX_SYSROOT \"{}\")\n",
            cmake_quote(&sdk.to_string_lossy())
        ));
    }
    let path = dir.join("probe-toolchain.cmake");
    fs::write(&path, content)
        .with_context(|| format!("writing probe toolchain file {}", path.display()))?;
    Ok(path)
}

fn cmake_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Literally the same environment policy as dependency builds
/// (cmake_build::scrubbed_env's allowlist): a host variable reaching only
/// one of probe/build (SDKROOT, MACOSX_DEPLOYMENT_TARGET, ...) could make
/// the two flatten config-dependent genexes differently, so the probe
/// configure must see exactly what the build configure saw.
fn apply_scrubbed_env(cmd: &mut Command) {
    cmd.env_clear();
    cmd.envs(crate::cmake_build::scrubbed_env());
}

// ---------------------------------------------------------------------------
// Output collection + decoding
// ---------------------------------------------------------------------------

/// Concatenate probe-out/<i>.rec in index order. Every record inside a file
/// is RS-terminated, so plain concatenation cannot merge records across
/// files. A missing directory means find_package imported no new targets.
fn read_probe_outputs(dir: &Path) -> Result<String> {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(String::new()),
    };
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading probe output dir {}", dir.display()))?
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("rec") {
            continue;
        }
        let Some(index) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        files.push((index, path));
    }
    files.sort_by_key(|(i, _)| *i);
    let mut combined = String::new();
    for (_, path) in files {
        combined.push_str(
            &fs::read_to_string(&path)
                .with_context(|| format!("reading probe output {}", path.display()))?,
        );
    }
    Ok(combined)
}

/// Undo the configure-time transport escaping of the raw
/// INTERFACE_LINK_LIBRARIES value (see PROBE_TEMPLATE). An unrecognized
/// escape is passed through verbatim rather than erroring: it can only come
/// from a literal GS in a property value, which framing already survived.
fn decode_raw_transport(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != GS {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('G') => out.push(GS),
            Some('R') => out.push(RS),
            Some('U') => out.push(US),
            Some('D') => out.push('$'),
            Some(other) => {
                out.push(GS);
                out.push(other);
            }
            None => out.push(GS),
        }
    }
    out
}

fn tail_str(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        s.into_owned()
    } else {
        // Round down to a char boundary so slicing cannot panic.
        let mut start = s.len() - max;
        while !s.is_char_boundary(start) {
            start += 1;
        }
        format!("...{}", &s[start..])
    }
}

fn truncate_for_error(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_split_cmake_list_plain() {
        assert_eq!(split_cmake_list("a;b;c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn probe_split_cmake_list_escaped_semicolon() {
        assert_eq!(split_cmake_list(r"a\;b;c"), vec!["a;b", "c"]);
        assert_eq!(split_cmake_list(r"x\;"), vec!["x;"]);
    }

    #[test]
    fn probe_split_cmake_list_drops_empty_elements() {
        assert_eq!(split_cmake_list(""), Vec::<String>::new());
        assert_eq!(split_cmake_list(";;a;;b;"), vec!["a", "b"]);
        assert_eq!(split_cmake_list(";"), Vec::<String>::new());
    }

    #[test]
    fn probe_split_cmake_list_backslash_not_before_semicolon() {
        // A backslash not followed by ';' is an ordinary character.
        assert_eq!(split_cmake_list(r"a\b;c"), vec![r"a\b", "c"]);
    }

    #[test]
    fn probe_parse_records_roundtrip() {
        let raw = format!(
            "Fix::a{US}TYPE{US}STATIC_LIBRARY{RS}Fix::a{US}INTERFACE_SOURCES{US}{RS}"
        );
        let recs = parse_records(&raw).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].target, "Fix::a");
        assert_eq!(recs[0].property, "TYPE");
        assert_eq!(recs[0].value, "STATIC_LIBRARY");
        // Empty value is a valid record (missing property evaluated to "").
        assert_eq!(recs[1].property, "INTERFACE_SOURCES");
        assert_eq!(recs[1].value, "");
    }

    #[test]
    fn probe_parse_records_value_may_contain_semicolons() {
        let raw = format!("t{US}INTERFACE_COMPILE_DEFINITIONS{US}A=1;B=2{RS}");
        let recs = parse_records(&raw).unwrap();
        assert_eq!(recs[0].value, "A=1;B=2");
        assert_eq!(split_cmake_list(&recs[0].value), vec!["A=1", "B=2"]);
    }

    #[test]
    fn probe_parse_records_rejects_malformed() {
        assert!(parse_records(&format!("only-two-fields{US}TYPE{RS}")).is_err());
        assert!(parse_records(&format!("{US}TYPE{US}x{RS}")).is_err());
    }

    #[test]
    fn probe_decode_raw_transport_restores_genexes() {
        // "$<LINK_ONLY:DepA::depa>" as the CMake-side escaping emits it.
        let encoded = format!("{GS}D<LINK_ONLY:DepA::depa>");
        assert_eq!(decode_raw_transport(&encoded), "$<LINK_ONLY:DepA::depa>");
        let all = format!("{GS}G{GS}R{GS}U{GS}D");
        assert_eq!(decode_raw_transport(&all), format!("{GS}{RS}{US}$"));
        // Unknown escape passes through verbatim.
        assert_eq!(decode_raw_transport(&format!("{GS}Z")), format!("{GS}Z"));
    }

    #[test]
    fn probe_render_cmakelists_contains_expected_pieces() {
        let text = render_probe_cmakelists("fmt", BuildConfig::Debug);
        assert!(text.contains("find_package(fmt REQUIRED CONFIG)"));
        assert!(text.contains("set(CMAKE_BUILD_TYPE \"Debug\")"));
        assert!(text.contains("IMPORTED_LOCATION_DEBUG"));
        assert!(text.contains("MAP_IMPORTED_CONFIG_DEBUG"));
        assert!(text.contains("IMPORTED_LOCATION_NOCONFIG"));
        assert!(text.contains("INTERFACE_LINK_LIBRARIES_RAW"));
        // Genexes are only in the emitted content, driven by the props list.
        assert!(text.contains("$<TARGET_PROPERTY:${_cppkg_t},${_cppkg_p}>"));
    }

    #[test]
    fn probe_validate_find_name_rejects_injection() {
        assert!(validate_find_name("fmt").is_ok());
        assert!(validate_find_name("nlohmann_json").is_ok());
        assert!(validate_find_name("").is_err());
        assert!(validate_find_name("bad name").is_err());
        assert!(validate_find_name("bad\"name").is_err());
        assert!(validate_find_name("${evil}").is_err());
    }

    // Real-CMake end-to-end coverage (fixture install + probe) lives in
    // tests/probe_test.rs; the inline tests here stay fast and pure.
}
