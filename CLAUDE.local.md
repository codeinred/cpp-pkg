# CppPkg — continuity notes

## What this project is

CppPkg: a C++ package manager written in Rust, declarative `CppPkg.toml`
manifests. Design doc: `CPP_PKG.md`. Core differentiator: consuming existing
CMake projects by *extracting* a target/artifact manifest from them, via a
three-tier ladder:

1. CPS file if the package installs one
2. Probe imported targets from `<pkg>Config.cmake` via `find_package`
3. Probe buildsystem targets from configure for non-installable
   (FetchContent/add_subdirectory-style) projects

Two on-disk stores: raw download store, and a content-addressed artifact store
keyed on (package hash, build configuration).

## Documents & repo conventions

- `CPP_PKG.md` — concept/design doc (user-authored).
- `CPP_PKG_IMPLEMENTATION.md` — strategic implementation decision log,
  maintained by Claude (statuses: Decided / Leaning / Open); edit superseded
  decisions in place.
- `DESIGN_CHOICES.md` — fine-grained running log of concrete choices
  (especially Claude's autonomous ones, with rationale/oracle verdicts).
  **Must be committed whenever updated** (user instruction).
- Repo is git (user-initialized, branch `main`); user commits under
  "Alecto Irene Perez". `CLAUDE.local.md` is deliberately force-added/tracked
  here as a cross-instance continuity doc. `answers.txt` is the user's
  message-drafting scratch — leave untracked, don't delete.
- For contested design questions, spawn a subagent as an oracle and record
  the verdict in DESIGN_CHOICES.md (user instruction, 2026-08-13).

## Direction decisions (2026-08-13, from user)

- **Primary mode: CppPkg replaces the consumer's build system Cargo-style**
  (`CppPkg.toml` describes the consumer's libs/executables; CppPkg drives
  compilation).
- **Secondary mode (later): CMake invokes CppPkg as a dependency resolver**
  for existing CMake-heavy projects. Natural mechanism: CMake ≥3.24 dependency
  providers (`cmake_language(SET_DEPENDENCY_PROVIDER)` via
  `CMAKE_PROJECT_TOP_LEVEL_INCLUDES`, the cmake-conan pattern), with CppPkg
  emitting `<pkg>Config.cmake` shims from the manifest.
- Architecture framing agreed in discussion: the manifest is the central IR
  (CMake extraction as frontend; own build driver + CMake shim emission as
  backends). Suggested adopting CPS (with vendor extensions) as the manifest
  format itself. Suggested generating Ninja rather than driving compilation
  directly (incrementality, depfiles, P1689 modules dyndep for free).
  Toolchain: single source of truth in CppPkg, which generates the
  CMAKE_TOOLCHAIN_FILE used for dependency builds.
- Round-trip test idea: extract → emit shim → extract should hit a fixpoint.
- Sequencing note raised: the "secondary" CMake-provider mode exercises all
  the hard extraction/store work without needing the build driver, so it may
  be a good first shippable milestone.

## Design review status (2026-08-13)

Reviewed `CPP_PKG.md`; key open problems flagged to the user:

- Manifest extraction must capture full usage requirements (interface
  includes/defines/link libs, per-config IMPORTED_LOCATIONs), which means
  evaluating generator expressions (likely via `file(GENERATE)`), per config,
  and resolving target-to-target references recursively.
- Tier 3: file API codemodel gives build info, not consumer-facing interface
  requirements; build/source-tree paths get baked in → store entries are
  non-relocatable (Nix-style "paths are final" commitment needed); target names
  are un-namespaced and can collide.
- Transitive deps: must own CMAKE_PREFIX_PATH; diamond-dependency policy needed
  (C++ can't duplicate like Cargo — ODR/ABI).
- Config hash must include toolchain identity (Conan settings-vs-options
  lesson) and dependency artifact hashes (Nix-derivation-style).
- Lockfile should pin content hashes (tags are mutable).
- Out of scope to note: config files that export CMake functions/macros (Qt,
  CUDA) can't be captured in a manifest.

Prior art to consult: CPS spec, CMake experimental `install(PACKAGE_INFO)`,
Conan 2 package_id, Meson `cmake.subproject()`, Bazel `rules_foreign_cc`.

## Useful CMake mechanics established earlier

- Enumerate imported targets: diff directory property `IMPORTED_TARGETS`
  before/after `find_package` (≥3.9); needs real project context (script mode
  can't create imported targets).
- Enumerate buildsystem targets: recurse `SUBDIRECTORIES` +
  `BUILDSYSTEM_TARGETS` directory properties (≥3.7); defer to end of configure
  with `cmake_language(DEFER CALL ...)`.
- External enumeration: file API query `codemodel-v2`.
