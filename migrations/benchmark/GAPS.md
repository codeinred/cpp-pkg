# GAPS — google/benchmark v1.9.5 migration

Every friction point hit while porting benchmark's CMake build to
`CppPkg.toml`, keyed to the design questions. Severity is from this
project's perspective; "generalizes" notes why it will recur elsewhere.

## 1. Configure-time version stamping (codegen-escape-hatch, major)

Upstream runs `git describe --tags --match "v[0-9]*.[0-9]*.[0-9]*" --abbrev=8`
at configure time and injects the result as a per-source compile definition
(`BENCHMARK_VERSION="v1.9.5"` on `benchmark.cc` only).

- **cpp-pkg lacks**: any way to run a command at generation time and feed its
  output into a define/generated file. Workaround: hardcoded
  `'BENCHMARK_VERSION="v1.9.5"'` in the manifest — which now silently drifts
  if the pin moves.
- **Bonus finding (dependency mode)**: cpp-pkg's raw store checkout has **no
  `.git` directory**, so in declare-as-dependency mode upstream's
  `git describe` always fails and the build silently falls back to
  `project(VERSION)`. Here that coincidentally yields the same `v1.9.5`; pin
  a non-tag `rev` and the dependency reports a wrong version with no
  warning. Any escape hatch that "just runs git" won't work inside the store;
  the hatch needs access to lockfile facts (requested tag/rev, commit) as
  substitutable variables — for this whole class of project,
  `version-from = "git-tag"`-style metadata plus `${pin.tag}` substitution in
  defines would have sufficed, no arbitrary command execution needed.
- What the escape hatch must do for benchmark specifically: produce one
  string at manifest-evaluation time and attach it as a define. No files
  generated, no build-graph edges.

## 2. Compile/run feature probes (codegen-escape-hatch, major)

`cxx_feature_check(STD_REGEX / GNU_POSIX_REGEX / POSIX_REGEX / STEADY_CLOCK /
PTHREAD_AFFINITY / THREAD_SAFETY_ATTRIBUTES)` try_compile/try_runs tiny
programs and turns successes into global `-DHAVE_*` defines; also
`check_library_exists(rt shm_open)` gates `-lrt`.

- **cpp-pkg lacks**: any probe mechanism. Workaround: ran the reference CMake
  configure once, transcribed the macOS/arm64 outcome (`HAVE_STD_REGEX`,
  `HAVE_STEADY_CLOCK`, `HAVE_THREAD_SAFETY_ATTRIBUTES`; PTHREAD_AFFINITY and
  rt absent) as literal private defines.
- Consequence: the manifest is **platform-specific**. The same TOML is wrong
  on Linux (needs `HAVE_PTHREAD_AFFINITY`, possibly `rt`) — and there is no
  conditional syntax to even express "on Linux add X" (overlaps gap 4).
  Probes select the regex *backend source semantics* here, but in other
  projects they select source files outright.
- Note: these probes are the moral equivalent of Cargo's `build.rs` +
  `cfg`; the schema will keep meeting them in every autotools/CMake-heritage
  codebase.

## 3. No per-target / per-source flags (per-target-flags, major)

Upstream flag structure that cpp-pkg cannot express:

- Warning battery (`-Wall … -Werror -pedantic-errors -Wthread-safety`) and
  visibility preset (`-fvisibility=hidden -fvisibility-inlines-hidden`) are
  directory-global for the library, but the `test/` directory *adds*
  suppressions (`add_cxx_compiler_flag(-Wno-unused-variable)`) and single
  tests override options (`donotoptimize_test`: `COMPILE_FLAGS "-O3"` +
  `-Werror=deprecated-declarations`).
- Per-source define: `BENCHMARK_VERSION` is attached to `benchmark.cc` only
  via `set_property(SOURCE … COMPILE_DEFINITIONS)`.
- `-Wsuggest-override` is added only when testing is off (option-conditional
  flag).

**cpp-pkg has**: `defines` per target (with visibility) but **no `cxx-flags`
on targets at all** — flags exist only on profiles, applied to every consumer
target identically. Workarounds used: (a) warning battery moved into profile
`cxx-flags`; (b) `-Wno-unused-but-set-variable` (needed only by
`test/basic_test.cc` under AppleClang 21 `-Werror`) applied globally —
documented deviation; (c) per-source version define widened to
target-private (safe here: consumed by one `#ifdef`).

Minimal fix that would have covered everything here: `cxx-flags` on targets
(private/public split like `defines`). Per-source properties were only needed
for the version define, which gap 1's fix subsumes.

## 4. Conditional sources / platform conditionals (conditional-sources, major)

- Upstream computes `SOURCE_FILES` = `glob(src/*.cc)` **minus**
  `benchmark_main.cc`. cpp-pkg globs have no exclusion syntax → 19 files
  listed by hand (drift hazard when upstream adds a file: silently missing
  symbol at link, not a manifest error). Wanted: `sources = ["src/*.cc",
  "!src/benchmark_main.cc"]` or similar.
- Platform-conditional link libs/defines have no home: `shlwapi` (Windows),
  `kstat` (Solaris), `rt` (Linux), `-D_GNU_SOURCE` (Cygwin), MSVC vs GCC
  flag branches. The manifest can only encode one platform's truth; there is
  no `[target.'cfg(...)']`-style select. For a *macOS-only* migration this
  cost nothing, which is exactly why it's easy to underestimate.

## 5. Test suite inexpressible (testing-story, major — scope reduction)

Not migrated (equivalent to `BENCHMARK_ENABLE_TESTING=OFF`):

- **Test-only dependency**: googletest, either bundled-downloaded
  (`BENCHMARK_USE_BUNDLED_GTEST` via ExternalProject at configure time) or
  `find_package(GTest)`. cpp-pkg has no dev-dependencies section, so
  declaring gtest would poison the production dependency closure.
- **Runner**: ~40 CTest registrations (some with `--benchmark_min_time=0.01`
  args, output-checked "output tests"); no `cpp-pkg test` exists.
- **Per-test build tweaks**: tests scrub `-DNDEBUG` in release
  (`add_definitions(-UNDEBUG)` — an *un*-define, also inexpressible),
  per-test flags (gap 3), and a `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS`
  define.
- What was salvageable without any of that: `test/basic_test.cc` compiles as
  a plain executable target and doubles as the smoke benchmark — that is the
  only fraction of the suite reachable today.

## 6. No install/export story (install-export, major)

Upstream installs: both archives (with `VERSION`/`SOVERSION` properties),
headers, `benchmarkConfig.cmake` + version file + targets export,
`benchmark.pc`/`benchmark_main.pc` (with a derived `Libs.private` line), and
python tools. The cpp-pkg source-mode build produces `build/libbenchmark.a`
et al. and **stops** — no `cpp-pkg install`, no way to emit a config/CPS
file for the targets this manifest defines. Consequence observed directly:
the only way to let another project consume this migration is to *not use
the migration* and declare upstream as a CMake dependency instead (which is
what `consumer/` does). A manifest-driven `install`/`package` command
emitting CPS or a Config.cmake from the target graph would close the loop —
and the round-trip (cpp-pkg-built benchmark consumed via cpp-pkg extraction)
is a natural fixpoint test.

## 7. System libraries / Threads (dep-provisioning, minor here)

Upstream links `Threads::Threads` (PRIVATE). On macOS pthreads live in
libSystem so omitting it is *correct on this platform only* — the manifest
has no way to say "system threads" (or `-pthread` where required), so a
Linux port would need to smuggle `-pthread` through profile flags. Same
shape as the `rt` case in gap 2. A small vocabulary of well-known system
capabilities (`threads`, `dl`, `m`, `rt`) would cover most of what CMake
find-modules provide here. Notably the *dependency* mode has no such
problem: the installed config's `find_dependency(Threads)` runs inside the
probe and the resulting interface came through cleanly.

## 8. Schema ergonomics (schema-ergonomics, minor)

- **Profile flag duplication**: identical 20-flag `cxx-flags` lists pasted
  into `[profiles.release]` and `[profiles.debug]` because flags common to
  all profiles have no home (`[profiles.all]`? top-level `cxx-flags`?).
- **Repeated `cxx-std = 17`** on every target; a `[package]`-level default
  (upstream's `CMAKE_CXX_STANDARD`) would remove three copies.
- **Quoted define values worked first try** (`'BENCHMARK_VERSION="v1.9.5"'`
  survived TOML → ninja → shell intact and matched CMake's escaping) — worth
  a doc example, since it's the thing a migrator is least sure about.
- **Good news data point**: the whole port needed zero patches and the
  build-graph part of the schema (visibility splits, static-lib private
  propagation, bare-list sugar) mapped 1:1 onto upstream's structure; every
  gap above is about what *surrounds* the target graph (probes, codegen,
  tests, install), not the graph itself.
