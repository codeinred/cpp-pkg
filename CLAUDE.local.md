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

## Implementation status (2026-08-14, post-integration)

All 12 modules + CLI implemented and integrated; `cargo build`, `cargo test`
(176 tests), and `cargo clippy --all-targets` are clean. Verified end-to-end
by hand: (1) hello-world executable; (2) static-library + executable with
public include/define propagation and header-touch incremental rebuilds;
(3) local-git CMake dep (fetch → cmake build → probe → manifest → link),
store cache hit on rebuild, lockfile written; (4) `cpp-pkg provide` +
dependency-provider consumer (CMake 3.24 SET_DEPENDENCY_PROVIDER) end-to-end;
(5) `needs` chain with transitive find_dependency and transitive shim
emission.

Integration decisions worth remembering:

- Object files live under `<target>.dir/` (CMake convention) — a bare
  `<target>/` directory collides with an executable output named `<target>`.
- `provide` prints ONLY the shim dir on stdout (provider script captures it);
  every subprocess in the pipeline captures its own stdout, keep it that way.
- `provide` also emits shims for the dep's `needs` closure into the same dir
  and appends `include()` lines to the main Config shim (find_dependency
  equivalent; shims are idempotent via if(NOT TARGET) guards).
- manifest::from_probe evaluates `SHELL:` groups in INTERFACE_LINK_OPTIONS
  into words; `SHELL:-framework X` returns to the frameworks bucket (closes
  the shim round-trip).
- ABI profile flags fold into dep config hashes in profile order (cxx, c,
  link lists; deduped) — see cli::profile_abi_flags.
- Known deferred: probe toolchain file carries no ABI flags (interface
  extraction doesn't depend on them); IMPORTED_LINK_INTERFACE_LANGUAGES not
  round-tripped; paths serialize via to_string_lossy; scrubbed_env drops
  DEVELOPER_DIR/SDKROOT (fine with /usr/bin shims, revisit for xcode-select).
- Machine fact correction: Homebrew has g++-16 (not g++-15 as older notes
  said); toolchain tests probe PATH for any g++-N and skip if absent.

Post-review fixes (2026-08-14, see DESIGN_CHOICES.md "Post-review fix
decisions" for the semantics): LINK_ONLY bare libs classify into ordinary
buckets (BLOCKER — CMake exports PRIVATE deps of static libs as
$<LINK_ONLY:m> etc.); MAP_IMPORTED_CONFIG has full precedence and no
fallback when unsatisfied; provider script forwards CMAKE_BUILD_TYPE as
--config; §5 find-registry off + CMakeCache leak scan in dep build AND
probe; probe now uses cmake_build::scrubbed_env (same allowlist as builds);
-stdlib=* reaches CXX_FLAGS_INIT only; ninja links archives via $libs
(interleaved plan order kept) with implicit deps; lockfile pruned + saved
per-resolution; gitlink (160000) submodule detection; git env
(GIT_DIR/...) scrubbed, checkout forces autocrlf=false; interface-source
units dedup by object path; object paths with ../ segments use the hashed
branch; naming-ladder step 2 decisive; renames colliding with project
target names error. Still deferred: probe toolchain file carries no ABI
flags; store entries are not fsynced tree-wide before mark_complete
(power-loss window); raw-store marker still lives inside the source tree.
NOTE: manifests cached in existing stores before this fix may still carry
bare libs in link_requires — wipe the store (or bump nothing; hashes are
unchanged) if a PLAN ERROR `unknown dependency reference` appears.

## Useful CMake mechanics established earlier

- Enumerate imported targets: diff directory property `IMPORTED_TARGETS`
  before/after `find_package` (≥3.9); needs real project context (script mode
  can't create imported targets).
- Enumerate buildsystem targets: recurse `SUBDIRECTORIES` +
  `BUILDSYSTEM_TARGETS` directory properties (≥3.7); defer to end of configure
  with `cmake_language(DEFER CALL ...)`.
- External enumeration: file API query `codemodel-v2`.
