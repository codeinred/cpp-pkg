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
