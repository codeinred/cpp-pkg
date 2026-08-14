# CppPkg — Design Choices

Running log of concrete design choices, especially those made autonomously by
Claude during implementation (with rationale, and oracle-subagent verdicts
where one was consulted). Strategy-level decisions live in
`CPP_PKG_IMPLEMENTATION.md`; this file is the finer grain. Committed on every
update.

---

## 2026-08-13 — Prototype scope rulings (user)

- **Vertical slice:** a small family of test projects, each exercising one
  component of the design: (a) a single executable; (b) a library; (c) a
  project with multiple executables & libraries whose dependencies are
  non-trivial (require explicit CMake configuration to build); (d) a project
  exercising transitive-dependency provision *(exact shape pending
  clarification — see Open)*. All dependencies are real-world C++ projects
  representative of the ecosystem.
- **Platform:** macOS/arm64 now. User picks up Linux once the initial version
  exists. GCC is testable on this machine (Homebrew).
- **Extraction:** Tier 2 first.
- **Configs:** built in *early* and threaded through dependencies — a Debug
  project builds its dependencies in Debug. (Config is part of the artifact
  config hash.)
- **Dep linkage:** static by default; shared-library support is on the TODO.
- **Tools:** system cmake + ninja; unconditional build.ninja regeneration in
  v0; C sources supported alongside C++; C++20 modules deferred.
- **Dependency closure:** user declares the full closure in `CppPkg.toml`;
  CppPkg builds deps in dependency order and errors clearly when a
  `find_dependency` cannot be satisfied from the store.
- **Toolchain UX:** auto-detect `c++` on PATH by default; CLI override; named
  toolchain presets definable in `CppPkg.toml`.
- **Lockfile exists from day one.**
- **Single-config generators only** for dependency builds (Ninja by default);
  multi-config generators unsupported for now.

## 2026-08-13 — Choices made by Claude

- **Doc split:** this file = fine-grained running choices;
  `CPP_PKG_IMPLEMENTATION.md` = strategic decision log. (Happy to merge them
  if the split proves annoying.)
- **.gitignore:** `logs/`, `.cppkg/` (consumer build trees), `target/`.
- **Format versioning:** the manifest and both store layouts carry an explicit
  schema-version field from day one — cheap insurance against migration pain.
- **Testing:** integration tests that hit the network (git clones of real
  deps) are separated/marked from fast tests; small vendored fixtures for the
  fast path.
- **Scope:** non-CMake dependencies (autotools, meson, plain-makefile) are
  explicitly out of scope for v0; clear error on encounter.
- **Config propagation policy:** dependency config always equals consumer
  config in v0 (no per-dep override yet — mixed Debug/Release is a footgun:
  `NDEBUG`-guarded ABI differences, `_GLIBCXX_ASSERTIONS`, MSVC runtimes
  later). A per-dependency override knob is future work.

## 2026-08-13 — Provider mode in v0 (user)

Test project (d) uses the CMake ≥3.24 dependency-provider mode: a CMake-built
consumer resolving deps through CppPkg via `SET_DEPENDENCY_PROVIDER`. This
pulls **Config-shim emission** into the prototype and unlocks the
extract→emit→extract fixpoint test early. (Transitive `find_dependency`
resolution via `CMAKE_PREFIX_PATH` is exercised independently in primary mode
by the spdlog→fmt chain, regardless of this choice.)

## 2026-08-13 — CppPkg.toml v0 schema: oracle review integrated

An oracle subagent adversarially reviewed the draft schema (5 contested
points). Verdicts: all ACCEPT-WITH-CHANGES, no rejections. Changes adopted
into `CPPKG_TOML.md` (now normative):

- **Naming:** resolution ladder from `CPP_PKG.md` incorporated (unique →
  `<depkey>::` prefix → `exposes-namespace`/`exposes-targets`, mapping form
  renames); ambiguity is a hard error listing candidates; dep keys and target
  names restricted to `[a-zA-Z0-9_-]+` reserving qualifier syntax;
  `dependencies` arrays specced string-or-table (strings-only implemented in
  v0).
- **Visibility:** static-library `private` deps propagate as link-only edges
  ($<LINK_ONLY> semantics) — compile reqs stop, artifacts reach the final
  link closure. Bare-list sugar uniform across includes/defines/deps.
- **`needs`:** CMAKE_PREFIX_PATH gets the *transitive closure*; entries
  validated against [dependencies]; cycles error; feeds config hash;
  not-found AND version-rejection find_dependency failures both translated.
- **Profiles:** consumer-only flags kept for v0, rationale corrected to
  scope/store-churn (not "hash meaningfulness"); hard-error denylist for
  ABI-affecting flags (_GLIBCXX_DEBUG etc.); warning for -fsanitize=*;
  `base-config` reserved for future custom profiles; evolution path (opt-in
  flags-to-deps must fold into dep config hash) recorded.
- **Languages:** exhaustive extension table, unknown ext = hard error; `.C`
  hard error (case-insensitive FS); `.m/.mm` clear unsupported error; link
  language rule (any C++ in closure → C++ driver); `c-flags` added;
  `cxx-extensions` reserved with default **false** (strict `-std=`) decided
  now.
- **Lockfile:** `source`/`requested` grammar pinned as ABI; **canonical
  content-hash defined** (url = archive bytes; git = sorted-path tree
  serialization, exec-bit-only modes, no mtimes, no .git); **git submodules
  are an error in v0**. Oracle flagged canonicalization as the single most
  cornering item — fixed before any lockfile ships.
- **Also recorded:** CMake `options` hashed as literal strings (never
  normalize ON/TRUE/1); source globs resolve in sorted byte order; `path`
  dependencies' future shape (bypass store, always rebuild) written down so
  store immutability assumptions don't foreclose them.
- **Sync with updated CPP_PKG.md** (user extended it): binary is `cpp-pkg`
  (hyphenated); default build dir `./build` + compile_commands.json;
  `--query` and `--path/--with` prototyping flows noted. Key convention
  normalized to kebab-case (`exposes-namespace` vs concept doc's snake_case)
  — user may veto.

## 2026-08-13 — User rulings on schema follow-ups

- **Git content hash = commit sha for v0.** Supersedes the canonical
  tree-serialization spec (kept on record only as a future fallback, e.g. if
  git-independent store verification is ever needed). The lockfile `commit`
  field is simultaneously pin, integrity check (`git rev-parse` after
  checkout; hardened SHA-1 acceptable for v0), and the re-download reference
  for fresh machines. `content-hash` now appears only on url sources.
  Submodules-error-in-v0 unchanged.
- **ABI-affecting profile flags fold into dependency config hashes** and
  propagate to dependency builds (via the generated toolchain file), instead
  of hard-erroring — the classification table needed for the denylist drives
  correct propagation instead. Deps rebuild under such profiles by
  construction. Non-ABI flags stay consumer-only; `-fsanitize=*` stays
  consumer-only + warning (sanitizers interoperate with uninstrumented
  code by design).
- **kebab-case keys approved** for the initial schema; may flip later on
  aesthetics.

## 2026-08-13 — Implementation kickoff choices (Claude)

- **Crate layout:** single crate, bin `cpp-pkg` + lib `cppkg`, edition 2024
  (user instruction);
  module contracts frozen in stubs (src/*.rs doc comments are normative for
  implementers). Deps pre-declared in Cargo.toml; agents may not edit it.
- **Probe wire format:** record-oriented text (\x1E record sep, \x1F field
  sep), NOT JSON — CMake cannot safely JSON-escape arbitrary property
  values; `;`-list splitting happens in Rust. `$<LINK_ONLY>` preserved via a
  parallel raw (unevaluated) INTERFACE_LINK_LIBRARIES record.
- **Schema addition:** optional dependency field `find-package` (probe's
  find_package name when it differs from the dep key, e.g. key `json` →
  `nlohmann_json`). Defaults to the dep key. (User may veto.)
- **Store entry crash-safety:** `.cppkg-entry.toml` marker with
  `complete = true` written last; incomplete entries treated as absent.
- **CMake 4.x host note:** dep configures pass
  `CMAKE_POLICY_VERSION_MINIMUM=3.5` (CMake 4.4 dropped <3.5 compat and many
  real deps still declare old minimums).
- **Provider mechanism detail:** provider script shells out to an internal
  `cpp-pkg provide` subcommand and find_package's the emitted shim
  (NO_DEFAULT_PATH); FIND_PACKAGE method only in v0 (FetchContent deferred).
- **`--path/--with` prototyping flow deferred post-v0.**

## 2026-08-14 — Post-review fix decisions (Claude)

Two independent reviews (correctness + conformance) drove these; semantics
worth pinning:

- **LINK_ONLY classification:** the inner value of `$<LINK_ONLY:x>` is
  classified like any other link entry; only *target references* land in the
  manifest's `link_requires` (the link-only edge kind). Bare libs (`m`,
  `-lz`), absolute paths, and frameworks go to their ordinary buckets — those
  carry no compile requirements, so they are link-only by construction, and
  treating them as component names crashed graph resolution (CMake exports
  every PRIVATE dep of a static library this way). Consequence: a re-emitted
  shim spells them unwrapped; extract→emit→extract still reaches a fixpoint
  after the first normalization.
- **MAP_IMPORTED_CONFIG precedence:** a set map has full precedence over
  `IMPORTED_LOCATION_<CONFIG>`, and a set-but-unsatisfied map reads as
  not-found (no fallback) — matches CMake 4.4 behavior, verified by
  experiment during review.
- **§5 find-control/leak detection (initial cut):** every configure cpp-pkg
  runs (dep build + probe) passes `CMAKE_FIND_USE_{,SYSTEM_}PACKAGE_REGISTRY
  =OFF` and `CMAKE_FIND_USE_CMAKE_ENVIRONMENT_PATH=OFF`
  (PATH-based find_program stays enabled), and after a successful configure
  the `CMakeCache.txt` is scanned: any `<pkg>_DIR` whose directory really
  holds a `<pkg>Config.cmake`/`<pkg>-config.cmake` must lie under the store
  prefixes / the dep's own trees, else a hard error suggests adding the
  package to `[dependencies]` + `needs`. Full `--debug-find` parsing remains
  future work; module-mode results (`Find*.cmake`) are deliberately not
  policed yet.
- **Provider mode config:** the provider script forwards the consumer's
  `CMAKE_BUILD_TYPE` (lowercased; empty → release) as `cpp-pkg provide
  --config`, closing the Debug-consumer-links-Release-artifacts hole.
- **ABI flag routing to deps:** `-stdlib=*` is written only to
  `CMAKE_CXX_FLAGS_INIT` in the generated toolchain file (C driver warns
  "argument unused", fatal under -Werror); the rest of the ABI class goes to
  both. The config-hash input stays the merged list (no store invalidation).
- **Submodule detection:** gitlink entries (`git ls-files -s` mode 160000)
  are checked in addition to `.gitmodules` — a committed gitlink without
  `.gitmodules` must still hard-error.
- **Lockfile hygiene:** entries are pruned when their dep key leaves
  `CppPkg.toml` (full builds only, not `provide`'s closure slice), and the
  lockfile is saved after each resolution rather than only at the end (a
  mid-build failure no longer discards pins already used to build store
  entries).
- **Naming ladder step 2 is decisive:** when a reference's `<prefix>::`
  names an existing dependency and the name is not unique, ownership is
  decided there — export it or error with that dep's export list; it never
  falls through to exposes-* claims by other packages. An `exposes-targets`
  rename colliding with a project target name is now a hard error (sibling
  names win in resolution, so the rename could never be referenced).

## 2026-08-14 — Test-project family green; findings from real dependencies

All four v0 test projects (tests/projects/) went green on the FIRST round —
no fix cycles: exe-fmt (fmt 11.2.0), lib-json (nlohmann_json v3.12.0,
Interface extraction + public/private propagation proven via --query),
multi-curl-spdlog (spdlog SPDLOG_FMT_EXTERNAL + needs=[fmt] transitive
find_dependency; curl-8_14_1 static with full options set — LINK_ONLY bare
libs, macOS frameworks, SDK zlib .tbd all extracted correctly; binaries link
against zero Homebrew dylibs), provider-consumer (CMake 3.24
SET_DEPENDENCY_PROVIDER end-to-end, store-cache reconfigure 0.24s).

Decisions/limitations recorded from the campaign:

- **New subcommand `cpp-pkg provider-script --dir <d>`** emits
  cppkg_provider.cmake (binary path baked in; machine-specific, gitignored).
  Closes the gap where only a unit test invoked shim::write_provider_script.
- **Known limitation (tier-2): ALIAS imported targets are invisible** to the
  IMPORTED_TARGETS diff — e.g. curl's `CURL::libcurl` is an alias of
  `CURL::libcurl_static`; users must reference the underlying imported name.
  The failure error is clear and the store manifest makes diagnosis easy.
  Future fix idea: scan config files for add_library(... ALIAS) during probe.
- **Project-level lesson (documented in multi-curl-spdlog README):** curl
  8.15 removed Secure Transport; CURL_USE_SECTRANSP silently becomes unused
  and configure falls back to Homebrew OpenSSL (non-hermetic). Pin
  curl-8_14_1 or use CURL_ENABLE_SSL=OFF. Hermeticity checks that would
  catch this automatically (store manifest referencing paths outside store +
  SDK) are a good future guard.
- Minor extraction observations (non-blocking, logged for later): manifest
  include paths can appear duplicated per component (deduped downstream);
  nlohmann's config also defines an un-namespaced `nlohmann_json` component
  (faithful to upstream, kept).
- Test projects keep their CppPkg.lock committed (deliberate — exercises the
  lockfile path); build/ dirs and generated provider scripts are gitignored;
  per-project .clangd points at build/compile_commands.json so editors
  resolve includes.

## Open
- Test dependency shortlist. Proposed: **fmt** (clean, simple installed lib),
  **spdlog** with `SPDLOG_FMT_EXTERNAL=ON` (real transitive
  `find_dependency(fmt)`), **nlohmann_json** (header-only INTERFACE target),
  **libcurl** (non-trivial required configuration + optional transitive
  find_dependency on zlib/OpenSSL). To finalize during test-matrix design.
- CLI surface for v0: `cpp-pkg build [target(s)]` with `--config
  <debug|release|...>`, `--toolchain <name|path>`, `--query [path]`, and the
  `--path <file> --with <dep>...` prototyping flow (per updated CPP_PKG.md).
  Flag details to finalize during implementation.
- Probe-project template design (tier 2) — next design work item.
