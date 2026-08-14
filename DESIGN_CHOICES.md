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

## Open
- Test dependency shortlist. Proposed: **fmt** (clean, simple installed lib),
  **spdlog** with `SPDLOG_FMT_EXTERNAL=ON` (real transitive
  `find_dependency(fmt)`), **nlohmann_json** (header-only INTERFACE target),
  **libcurl** (non-trivial required configuration + optional transitive
  find_dependency on zlib/OpenSSL). To finalize during test-matrix design.
- `CppPkg.toml` v0 schema — next design work item (Claude, with oracle
  subagent for contested points).
- CLI surface for v0 (`cppkg build --config <debug|release>
  --toolchain <name|path>` as working assumption).
