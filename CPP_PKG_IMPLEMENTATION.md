# CppPkg — Implementation Decisions

This file records implementation-level decisions for CppPkg: what was decided,
why, and any alternatives that were rejected. The concept-level design lives in
`CPP_PKG.md`; this document is for the mechanics. Statuses: **Decided**,
**Leaning** (working assumption, revisit before it hardens), **Open**.

---

## 1. Operating modes

**Decided.** CppPkg has two modes:

1. **Primary — standalone build system.** `CppPkg.toml` describes the
   consumer's libraries/executables Cargo-style; CppPkg drives their
   compilation. Dependencies written in CMake are built via CMake and consumed
   through generated manifests.
2. **Secondary — CMake dependency provider.** For projects that already make
   substantial use of CMake, CppPkg can be invoked *by* CMake to resolve
   dependencies only. Ships later, but exercises the same extraction/store
   machinery.

## 2. The manifest is the central IR

**Decided.** All modes hang off the per-package manifest. CMake extraction
(tiers 1–3 in `CPP_PKG.md`) is a *frontend* producing manifests; the build
driver and the CMake shim emitter are *backends* consuming them.

**Decided (2026-08-13, explicitly revisitable).** Adopt the Common Package
Specification (CPS) as the on-disk manifest format, with vendor extensions
where CPS falls short (notably build-tree consumption paths for tier-3
packages), rather than inventing a private format. Rationale: tier 1 becomes a
no-op, CMake's experimental CPS support converges toward us, and interop comes
free. The user has flagged this as a reasonable-for-now choice that may change
later; keep the manifest access behind one Rust module boundary so a format
swap stays cheap.

The manifest must capture full **usage requirements**, not just target names:
interface include dirs, compile definitions/options/features, link libraries
(as resolved references to other manifest components, in topological order),
link options, and per-configuration artifact locations. Generator expressions
in probed CMake properties are flattened per configuration at extraction time
(via `file(GENERATE)`-style evaluation).

## 3. Toolchain identity and injection

**Decided.** CppPkg's own toolchain definition is the **single source of
truth**. From it, CppPkg *generates* the `CMAKE_TOOLCHAIN_FILE` passed to every
dependency's CMake configure. Dependency builds never pick up a compiler from
the environment on their own.

Rationale: consumer code is compiled by CppPkg while dependencies are compiled
by CMake; if the two could diverge, the artifact-store config hash would claim
two ABI-incompatible builds are identical.

Consequences:

- The configuration hash (see `CPP_PKG.md` store design) incorporates the
  toolchain identity: compiler path + version, target triple, stdlib, and any
  ABI-affecting global flags — not merely the package's own CMake options.
- The hash also incorporates the artifact hashes of the package's resolved
  dependencies (Nix-derivation-style), so rebuilding a dep invalidates
  dependents.
- Dependency configures run with a scrubbed/controlled environment to the
  extent practical; hermeticity is best-effort and documented as such.

## 4. CMake dependency-provider integration (secondary mode)

**Decided.** Use CMake's first-class dependency-provider mechanism (CMake ≥
3.24), the same pattern proven by Conan 2's cmake-conan:

- CppPkg ships a provider script; the consumer opts in with a single flag:
  `-DCMAKE_PROJECT_TOP_LEVEL_INCLUDES=<path>/cppkg_provider.cmake`
  (no edits to the consumer's `CMakeLists.txt`).
- The provider script calls
  `cmake_language(SET_DEPENDENCY_PROVIDER cppkg_provide
  SUPPORTED_METHODS FIND_PACKAGE FETCHCONTENT_MAKEAVAILABLE_SERIAL)`.
- On interception, CppPkg resolves the package from its artifact store and
  satisfies the request by emitting a `<pkg>Config.cmake` **shim** that
  recreates the manifest's components as imported targets, then points
  `find_package` at it.
- Minimum supported CMake for this mode is therefore 3.24.

**Round-trip invariant (test harness):** extraction and shim emission are
inverses — probing a shim emitted from a manifest must reproduce that manifest
(extract → emit → extract reaches a fixpoint). This is a standing correctness
test for both codepaths.

## 5. Dependency resolution inside dependency builds

**Decided.** When a dependency's own `FooConfig.cmake` calls
`find_dependency(Bar)`, resolution must land in CppPkg's store, not the
system: CppPkg owns `CMAKE_PREFIX_PATH` (and related find-control variables)
for every configure it runs, and uses find-debug output to detect leakage to
system packages.

**Decided (initial policy).** Diamond dependencies requiring *different
configurations* of the same package are an **error with a clear message**, not
silently duplicated — C++ ODR/ABI rules out Cargo-style duplication. A
unification strategy may come later.

## 6. Build execution (primary mode)

**Decided (2026-08-13).** Generate **Ninja** for consumer-code builds rather
than driving compilation directly. Rationale: correct incrementality, depfile
handling (`-MD`/`/showIncludes`), parallel scheduling, and C++20 modules
dependency scanning (P1689 → dyndep) come from mature machinery instead of
being reimplemented.

Ninja emission conventions (mirror CMake's, they are battle-tested):

- One `rule` per (toolchain, language, action); per-config flags folded into
  build statements, not rules.
- Header deps: `deps = gcc` + `-MD -MT $out -MF $out.d` for GCC/Clang;
  `deps = msvc` + `/showIncludes` + `msvc_deps_prefix` for cl (locale-dependent
  prefix — capture it at toolchain detection time).
- Long link lines on Windows via `rspfile`/`rspfile_content`.

**Decided (2026-08-13).** `$<LINK_ONLY:...>` **is supported**. In the manifest
IR it is simply an edge kind: a link-only requirement contributes its
artifacts to the consumer's link closure but propagates no compile
requirements. (This falls out naturally if compile-reqs and link-reqs are
separate fields on manifest edges — make them separate fields.)

**Decided (2026-08-13, reversing earlier scope cut).** `INTERFACE_SOURCES`
**is supported**: the manifest carries the injected source paths plus the
compile requirements they need; CppPkg compiles those sources as part of the
*consumer's* build graph. Store immutability makes this safe (the paths are
final). Extraction must record them per-config after genex flattening.

Still out of scope: config files whose value is exported CMake
functions/macros (Qt's `qt_add_resources`, CUDA helpers) — a manifest cannot
carry executable CMake code; fail with a clear message.

## 7. Lowering abstract requirements to flags

**Decided (2026-08-13, proposed by Claude — confirm before hardening).**
Lowering is a typed-IR → per-dialect-driver pipeline, not string pasting:

1. **Typed requirement IR.** Abstract requirements are Rust enums
   (`Std(Cxx20)`, `Include { path, system }`, `Define(k, v)`, `Pic`,
   `Framework(name)`, `LinkLib(ComponentRef | Path | Name)`, `RuntimeLib`,
   `LinkOption`, …). Free-form strings appear only at the final
   emission step. Unlowerable requirements are a hard error naming the
   requirement and the toolchain — never silently dropped.
2. **One driver per flag dialect, version-gated inside.** Reality has ~2.5
   dialects: **GNU-like** (GCC, Clang, Apple Clang, new Intel), **MSVC-like**
   (cl, clang-cl), and Apple/link-time quirks handled inside the GNU driver
   (frameworks, `-Wl,-rpath,`). A `ToolchainDriver` trait with these impls,
   keyed by (dialect, version); genuinely tabular parts (std→flag,
   config→default flags) live in data tables mined from CMake's
   `Modules/Compiler/*.cmake` (BSD-3, derivable with attribution).
3. **Toolchain detection via predefined macros**, not version-banner parsing:
   `<cc> -dM -E -x c++ /dev/null` → `__clang_major__`, `__GNUC__`, …; cl
   detected via its banner/`_MSC_VER` probe. Also capture default
   include/lib search dirs (`-v`, `-print-search-dirs`) for leakage checks,
   and cl's `/showIncludes` prefix. Two distinct keys, deliberately different
   (discussed 2026-08-13):
   - *Detection cache* (re-run probes?): stat-based key (path, size, mtime),
     ccache-style; opt-in content-hash mode for hosts with untrustworthy
     mtimes. Never hash the binary by default — cost without benefit.
   - *Toolchain identity in the config hash (§3)*: the **detection output**,
     normalized — dialect, vendor, exact version from predefined macros,
     target triple, default stdlib + version. NOT a binary hash: too strong
     (same version at a new path/re-signing invalidates the store, breaks any
     future shared cache) and too weak (gcc/clang are drivers; the hash misses
     cc1plus, ld, headers, sysroot, stdlib). **Decided (2026-08-13):**
     normalized semantic identity only; no strict/content-hash mode for now
     (the vendor-patched-compiler case is not worth the surface area yet).
4. **Config names stay CMake-compatible** (Debug/Release/RelWithDebInfo/
   MinSizeRel) so per-config manifest data lines up with
   `IMPORTED_LOCATION_<CONFIG>` from dependency builds.
5. **Link lowering:** topological order over the manifest component graph;
   dedup static archives keeping the *last* occurrence; declared cycles →
   `--start-group`/`--end-group` (GNU) with an explicit manifest cycle
   annotation, mirroring CMake's link-feature approach.

Reference: how CMake stores this same knowledge — per-compiler variable tables
in `Modules/Compiler/<Id>-<Lang>.cmake` + `Modules/Platform/`, rule templates
(`CMAKE_CXX_COMPILE_OBJECT` etc.) with `<PLACEHOLDER>` substitution, compiler
identification by compiling `CMakeCXXCompilerId.cpp` and scraping
`INFO:compiler[...]` strings from the binary, ABI/implicit-path detection via
a verbose probe link. We take the data, not the architecture (see discussion
2026-08-13).

## 8. Stores and pinning

(Extends the store design in `CPP_PKG.md`.)

- **Decided.** Tier-3 store entries (source tree + build tree) are
  **non-relocatable**: configure bakes absolute paths, so store paths are
  final once created — the Nix commitment. No path rewriting.
- **Decided.** A lockfile pins content hashes for every raw download, even
  when the user specified a mutable ref (git tag, branch); the raw download
  store verifies hashes on use.

## 9. Extraction mechanics (reference)

Established CMake techniques the extractor is built on:

- **Tier 2 (installed, config-file packages):** diff the `IMPORTED_TARGETS`
  directory property before/after `find_package` inside a throwaway probe
  project (script mode cannot create imported targets). Transitive
  `find_dependency` targets appear in the diff — desirable, but attribute them
  to their own packages by namespace where possible.
- **Tier 3 (non-installable projects):** enumerate via recursion over the
  `SUBDIRECTORIES` + `BUILDSYSTEM_TARGETS` directory properties, deferred to
  end of configure with `cmake_language(DEFER CALL ...)`; the file API
  (`codemodel-v2`) supplies build-side detail but *not* consumer-facing
  interface requirements, which still come from property probing.
- **Usage requirements:** probe interface properties per target; flatten
  generator expressions per configuration via `file(GENERATE)`; resolve
  target references in `INTERFACE_LINK_LIBRARIES` recursively into manifest
  component references. Tier-3 target names are un-namespaced; the manifest
  normalizes naming across tiers.

---

*Maintained by Claude across sessions; see `CLAUDE.local.md` for continuity
notes. When a decision here changes, edit it in place and note the
supersession rather than appending a contradicting entry.*
