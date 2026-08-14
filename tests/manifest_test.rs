//! Public-API tests for cppkg::manifest: probe-record classification,
//! imported-location fallback order, LINK_ONLY handling, and JSON
//! round-trip stability.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cppkg::manifest::{from_probe, Component, ComponentKind, Manifest};
use cppkg::probe::ProbeRecord;
use cppkg::schema::BuildConfig;

fn rec(target: &str, property: &str, value: &str) -> ProbeRecord {
    ProbeRecord {
        target: target.to_string(),
        property: property.to_string(),
        value: value.to_string(),
    }
}

fn build(records: &[ProbeRecord]) -> Manifest {
    from_probe("dep", "Dep", BuildConfig::Release, records).unwrap()
}

#[test]
fn manifest_kind_mapping() {
    let m = build(&[
        rec("a::static", "TYPE", "STATIC_LIBRARY"),
        rec("a::shared", "TYPE", "SHARED_LIBRARY"),
        rec("a::iface", "TYPE", "INTERFACE_LIBRARY"),
        rec("a::unknown", "TYPE", "UNKNOWN_LIBRARY"),
        rec("a::exe", "TYPE", "EXECUTABLE"),
    ]);
    assert_eq!(m.components["a::static"].kind, Some(ComponentKind::Archive));
    assert_eq!(m.components["a::shared"].kind, Some(ComponentKind::Dylib));
    assert_eq!(m.components["a::iface"].kind, Some(ComponentKind::Interface));
    assert_eq!(m.components["a::unknown"].kind, Some(ComponentKind::Unknown));
    assert_eq!(m.components["a::exe"].kind, None);
    assert!(m.notes.iter().any(|n| n.contains("EXECUTABLE")));
    // Interface libraries with no location must not produce a warning.
    assert!(!m.notes.iter().any(|n| n.contains("a::iface")));
    assert_eq!(m.package, "dep");
    assert_eq!(m.components["a::static"].origin_find_name, "Dep");
}

#[test]
fn manifest_location_exact_config_wins() {
    let m = build(&[
        rec("d::d", "TYPE", "STATIC_LIBRARY"),
        rec("d::d", "IMPORTED_LOCATION_RELEASE", "/s/lib/librel.a"),
        rec("d::d", "IMPORTED_LOCATION", "/s/lib/libplain.a"),
        rec("d::d", "IMPORTED_CONFIGURATIONS", "Debug"),
        rec("d::d", "IMPORTED_LOCATION_DEBUG", "/s/lib/libdbg.a"),
    ]);
    assert_eq!(
        m.components["d::d"].location["Release"],
        PathBuf::from("/s/lib/librel.a")
    );
}

#[test]
fn manifest_location_map_imported_config() {
    let m = build(&[
        rec("d::d", "TYPE", "STATIC_LIBRARY"),
        rec("d::d", "MAP_IMPORTED_CONFIG_RELEASE", "MinSizeRel;Debug"),
        rec("d::d", "IMPORTED_LOCATION_DEBUG", "/s/lib/libdbg.a"),
        rec("d::d", "IMPORTED_LOCATION", "/s/lib/libplain.a"),
    ]);
    // First map entry (MinSizeRel) has no location; second (Debug) does.
    assert_eq!(
        m.components["d::d"].location["Release"],
        PathBuf::from("/s/lib/libdbg.a")
    );
}

#[test]
fn manifest_location_map_empty_entry_means_unsuffixed() {
    let m = build(&[
        rec("d::d", "TYPE", "STATIC_LIBRARY"),
        rec("d::d", "MAP_IMPORTED_CONFIG_RELEASE", ";Debug"),
        rec("d::d", "IMPORTED_LOCATION", "/s/lib/libplain.a"),
        rec("d::d", "IMPORTED_LOCATION_DEBUG", "/s/lib/libdbg.a"),
    ]);
    assert_eq!(
        m.components["d::d"].location["Release"],
        PathBuf::from("/s/lib/libplain.a")
    );
}

#[test]
fn manifest_location_plain_fallback() {
    let m = build(&[
        rec("d::d", "TYPE", "STATIC_LIBRARY"),
        rec("d::d", "IMPORTED_LOCATION", "/s/lib/libplain.a"),
    ]);
    assert_eq!(
        m.components["d::d"].location["Release"],
        PathBuf::from("/s/lib/libplain.a")
    );
}

#[test]
fn manifest_location_imported_configurations_fallback() {
    let m = build(&[
        rec("d::d", "TYPE", "STATIC_LIBRARY"),
        rec("d::d", "IMPORTED_CONFIGURATIONS", "MinSizeRel;Debug"),
        rec("d::d", "IMPORTED_LOCATION_DEBUG", "/s/lib/libdbg.a"),
        rec("d::d", "IMPORTED_LOCATION_MINSIZEREL", "/s/lib/libmin.a"),
    ]);
    assert_eq!(
        m.components["d::d"].location["Release"],
        PathBuf::from("/s/lib/libmin.a")
    );
}

#[test]
fn manifest_location_missing_warns_for_libraries_only() {
    let m = build(&[
        rec("lib::lib", "TYPE", "STATIC_LIBRARY"),
        rec("hdr::hdr", "TYPE", "INTERFACE_LIBRARY"),
    ]);
    assert!(m.components["lib::lib"].location.is_empty());
    assert!(m
        .notes
        .iter()
        .any(|n| n.contains("lib::lib") && n.contains("no IMPORTED_LOCATION")));
    assert!(!m.notes.iter().any(|n| n.contains("hdr::hdr")));
}

#[test]
fn manifest_notfound_values_are_unset() {
    let m = build(&[
        rec("s::s", "TYPE", "STATIC_LIBRARY"),
        rec("s::s", "IMPORTED_LOCATION_RELEASE", "prop-NOTFOUND"),
        rec("s::s", "IMPORTED_LOCATION", "/s/lib/liba.a"),
    ]);
    assert_eq!(
        m.components["s::s"].location["Release"],
        PathBuf::from("/s/lib/liba.a")
    );
}

#[test]
fn manifest_link_only_parsing() {
    let m = build(&[
        rec("s::s", "TYPE", "STATIC_LIBRARY"),
        rec("s::s", "IMPORTED_LOCATION", "/s/lib/libs.a"),
        rec(
            "s::s",
            "INTERFACE_LINK_LIBRARIES_RAW",
            "fmt::fmt;$<LINK_ONLY:zlib::zlib>;$<LINK_ONLY:-lm>",
        ),
        rec("s::s", "INTERFACE_LINK_LIBRARIES", "fmt::fmt;zlib::zlib;-lm"),
    ]);
    let c = &m.components["s::s"];
    assert_eq!(c.requires, vec!["fmt::fmt"]);
    // Only target references stay in link_requires; a LINK_ONLY-wrapped bare
    // lib classifies into system_libs (which is link-only by construction)
    // instead of becoming a bogus component reference.
    assert_eq!(c.link_requires, vec!["zlib::zlib"]);
    assert_eq!(c.system_libs, vec!["m"]);
    assert!(m.notes.is_empty());
}

#[test]
fn manifest_link_only_nested_genex_warns() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_LINK_LIBRARIES_RAW",
            "$<LINK_ONLY:$<$<CONFIG:Debug>:dbg::only>>",
        ),
        rec("s::s", "INTERFACE_LINK_LIBRARIES", ""),
    ]);
    let c = &m.components["s::s"];
    assert!(c.link_requires.is_empty());
    assert!(m
        .notes
        .iter()
        .any(|n| n.contains("LINK_ONLY") && n.contains("dbg::only")));
}

#[test]
fn manifest_leftover_genex_in_evaluated_warns() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec("s::s", "INTERFACE_LINK_LIBRARIES", "$<TARGET_OBJECTS:zzz>"),
    ]);
    let c = &m.components["s::s"];
    assert!(c.requires.is_empty() && c.system_libs.is_empty());
    assert!(m.notes.iter().any(|n| n.contains("$<TARGET_OBJECTS:zzz>")));
}

#[test]
fn manifest_link_classification_buckets() {
    let m = build(&[
        rec("s::s", "TYPE", "STATIC_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_LINK_LIBRARIES",
            "fmt::fmt;/usr/lib/libz.a;-lm;dl;-pthread;-framework;CoreFoundation;-framework Metal;/System/Library/Frameworks/Cocoa.framework",
        ),
    ]);
    let c = &m.components["s::s"];
    assert_eq!(c.requires, vec!["fmt::fmt"]);
    assert_eq!(c.link_paths, vec![PathBuf::from("/usr/lib/libz.a")]);
    assert_eq!(c.system_libs, vec!["m", "dl"]);
    assert_eq!(c.link_options, vec!["-pthread"]);
    assert_eq!(c.frameworks, vec!["CoreFoundation", "Metal", "Cocoa"]);
}

#[test]
fn manifest_defines_split() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_COMPILE_DEFINITIONS",
            "FOO=1;BAR;BAZ=;QUX=a=b",
        ),
    ]);
    assert_eq!(
        m.components["s::s"].defines,
        vec![
            ("FOO".to_string(), Some("1".to_string())),
            ("BAR".to_string(), None),
            ("BAZ".to_string(), Some(String::new())),
            ("QUX".to_string(), Some("a=b".to_string())),
        ]
    );
}

#[test]
fn manifest_defines_escaped_semicolon_value() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec("s::s", "INTERFACE_COMPILE_DEFINITIONS", r"LIST=a\;b;PLAIN"),
    ]);
    assert_eq!(
        m.components["s::s"].defines,
        vec![
            ("LIST".to_string(), Some("a;b".to_string())),
            ("PLAIN".to_string(), None),
        ]
    );
}

#[test]
fn manifest_cxx_std_max_and_other_features_warn() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_COMPILE_FEATURES",
            "cxx_std_11;cxx_variadic_templates;cxx_std_17",
        ),
    ]);
    assert_eq!(m.components["s::s"].cxx_std, Some(17));
    assert!(m.notes.iter().any(|n| n.contains("cxx_variadic_templates")));
}

#[test]
fn manifest_includes_sources_options() {
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec("s::s", "INTERFACE_INCLUDE_DIRECTORIES", "/s/include;/s/other"),
        rec("s::s", "INTERFACE_SYSTEM_INCLUDE_DIRECTORIES", "/s/sys"),
        rec("s::s", "INTERFACE_COMPILE_OPTIONS", "-fexceptions"),
        rec("s::s", "INTERFACE_LINK_OPTIONS", "-Wl,-dead_strip"),
        rec("s::s", "INTERFACE_SOURCES", "/s/src/extra.cpp"),
    ]);
    let c = &m.components["s::s"];
    // Wave-1 A.1: imported interface include dirs classify as SYSTEM at
    // ingestion (declared order first, pre-marked system entries after).
    assert!(c.includes.is_empty());
    assert_eq!(
        c.system_includes,
        vec![
            PathBuf::from("/s/include"),
            PathBuf::from("/s/other"),
            PathBuf::from("/s/sys"),
        ]
    );
    assert_eq!(c.compile_options, vec!["-fexceptions"]);
    assert_eq!(c.link_options, vec!["-Wl,-dead_strip"]);
    assert_eq!(c.interface_sources, vec![PathBuf::from("/s/src/extra.cpp")]);
}

#[test]
fn manifest_interface_sources_skip_non_compilable() {
    // Wave-1 A.3: headers and IDE metadata in extracted INTERFACE_SOURCES
    // are skipped (CMake's own is-compilable classification); real sources
    // stay. Project sources are unaffected — this is extraction-only.
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_SOURCES",
            "/s/src/tz.cpp;/s/include/date/date.h;/s/misc/date.natvis;/s/src/ios.mm",
        ),
    ]);
    assert_eq!(
        m.components["s::s"].interface_sources,
        vec![PathBuf::from("/s/src/tz.cpp"), PathBuf::from("/s/src/ios.mm")]
    );
    assert!(m.notes.is_empty());
}

#[test]
fn manifest_link_options_shell_prefix() {
    // CMake evaluates SHELL: groups into words at generate time; the manifest
    // must do the same, and `-framework X` groups (the shim's own emission)
    // return to the frameworks bucket so extract -> emit -> extract closes.
    let m = build(&[
        rec("s::s", "TYPE", "INTERFACE_LIBRARY"),
        rec(
            "s::s",
            "INTERFACE_LINK_OPTIONS",
            "SHELL:-framework CoreFoundation;SHELL:-Xlinker -export_dynamic;-Wl,-x",
        ),
    ]);
    let c = &m.components["s::s"];
    assert_eq!(c.frameworks, vec!["CoreFoundation"]);
    assert_eq!(c.link_options, vec!["-Xlinker", "-export_dynamic", "-Wl,-x"]);
}

#[test]
fn manifest_json_round_trip_stability() {
    let m = build(&[
        rec("z::z", "TYPE", "STATIC_LIBRARY"),
        rec("z::z", "IMPORTED_LOCATION_RELEASE", "/s/lib/libz.a"),
        rec("z::z", "INTERFACE_INCLUDE_DIRECTORIES", "/s/include"),
        rec("z::z", "INTERFACE_COMPILE_DEFINITIONS", "Z_API=;Z_STATIC"),
        rec("z::z", "INTERFACE_COMPILE_FEATURES", "cxx_std_14;cxx_lambdas"),
        rec("z::z", "INTERFACE_LINK_LIBRARIES_RAW", "$<LINK_ONLY:-lm>"),
        rec("z::z", "INTERFACE_LINK_LIBRARIES", "-lm"),
        rec("a::a", "TYPE", "INTERFACE_LIBRARY"),
        rec("a::a", "INTERFACE_LINK_LIBRARIES", "z::z"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");

    m.save(&path).unwrap();
    let text1 = std::fs::read_to_string(&path).unwrap();
    assert!(text1.ends_with('\n'));
    assert!(text1.contains("\"schema_version\": 1"));
    assert!(text1.contains("\"name\": \"dep\""));

    let loaded = Manifest::load(&path).unwrap();
    assert_eq!(loaded, m);

    loaded.save(&path).unwrap();
    let text2 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text1, text2);
}

#[test]
fn manifest_save_load_manual_component() {
    // A hand-built manifest (every field populated) survives a round trip
    // modulo the wave-1 ingestion transforms `load` applies (A.1 moves the
    // includes bucket into system_includes on the way in).
    let comp = Component {
        kind: Some(ComponentKind::Dylib),
        location: BTreeMap::from([("Debug".to_string(), PathBuf::from("/s/lib/liba.dylib"))]),
        includes: vec![PathBuf::from("/s/include")],
        system_includes: vec![PathBuf::from("/s/sys")],
        defines: vec![("A".to_string(), None), ("B".to_string(), Some("2".to_string()))],
        compile_options: vec!["-fno-rtti".to_string()],
        cxx_std: Some(20),
        link_options: vec!["-Wl,-x".to_string()],
        requires: vec!["dep::other".to_string()],
        link_requires: vec!["dep::hidden".to_string()],
        link_paths: vec![PathBuf::from("/s/lib/libx.a")],
        system_libs: vec!["m".to_string()],
        frameworks: vec!["Cocoa".to_string()],
        interface_sources: vec![PathBuf::from("/s/src/i.cpp")],
        origin_find_name: "Dep".to_string(),
    };
    let m = Manifest {
        package: "dep".to_string(),
        components: BTreeMap::from([("dep::a".to_string(), comp)]),
        notes: vec!["a note".to_string()],
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.json");
    m.save(&path).unwrap();
    let loaded = Manifest::load(&path).unwrap();
    let mut expected = m.clone();
    cppkg::manifest::apply_ingestion_transforms(&mut expected);
    assert_eq!(loaded, expected);
    // The transformed shape is a fixpoint: saving and re-loading it is exact.
    loaded.save(&path).unwrap();
    assert_eq!(Manifest::load(&path).unwrap(), loaded);
}

#[test]
fn manifest_load_transforms_cached_v0_manifest() {
    // A store manifest written by the v0 extractor (includes in the plain
    // bucket, a literal Threads::Threads component and reference) converges
    // on read: -isystem classification + builtin rewrite (spec A.1, §5.4).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    std::fs::write(
        &path,
        r#"{
  "schema_version": 1,
  "name": "dep",
  "components": {
    "Threads::Threads": {
      "type": "interface",
      "link_options": ["-pthread"]
    },
    "dep::core": {
      "type": "archive",
      "location": { "Release": "/store/pkg/dep/install/lib/libcore.a" },
      "includes": ["/store/pkg/dep/install/include"],
      "requires": ["Threads::Threads", "dep::core"]
    }
  }
}
"#,
    )
    .unwrap();
    let m = Manifest::load(&path).unwrap();
    assert!(!m.components.contains_key("Threads::Threads"));
    let core = &m.components["dep::core"];
    assert!(core.includes.is_empty());
    assert_eq!(
        core.system_includes,
        vec![PathBuf::from("/store/pkg/dep/install/include")]
    );
    // Self-edge dropped, Threads reference rewritten to the builtin.
    assert_eq!(core.requires, vec!["builtin:threads"]);
}

#[test]
fn manifest_load_rejects_wrong_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    std::fs::write(
        &path,
        "{\"schema_version\": 99, \"name\": \"x\", \"components\": {}}\n",
    )
    .unwrap();
    let err = Manifest::load(&path).unwrap_err();
    assert!(err.to_string().contains("schema-version 99"));
}
