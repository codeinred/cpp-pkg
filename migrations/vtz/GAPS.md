# GAPS — vtz migration

Every friction point hit while porting voladynamics/vtz (main @ 8d6ea8f)
to cpp-pkg, keyed to the design-question lens. Status markers:
**blocker** (no in-manifest expression at all), **major** (workaround
exists but costs real correctness/ergonomics), **minor** (papercut).

---

## 1. codegen-escape-hatch — tzdb download / extract / embed (major)

**Upstream:** at configure time, top-level CMake always does
`file(DOWNLOAD tzdb-2026a.tar.lz)` → `file(ARCHIVE_EXTRACT)` →
`file(CREATE_LINK data/tzdata)`, downloads `windowsZones.xml` from cldr
*main* (unpinned!), and a `zic` `add_custom_command` compiles
`data/zoneinfo` that `test_vtz` depends on. Under `VTZ_REFRESH_TZDATA` it
additionally regenerates two checked-in headers from `tzdata.zi`:
`embedded_tzdb_content.h` (every line of tzdata.zi as a C string chunk)
and `known_zones.h` (`configure_file` with lists scraped from `Z `/`L `
lines).

**What cpp-pkg lacked:** any way to run a fetch/generate step. Nothing in
the manifest can produce a file, so the whole flow moved to
`scripts/fetch-tzdata.sh` (invoked by `pin.sh`, outside the build).

**Workaround quality:** good — *because vtz already checks the generated
headers in*, the library builds from a pure source tree, and the script
only has to provision test **runtime data**. `--verify-refresh`
re-implements the two generators (~20 lines of python) and proved the
checked-in headers reproduce **byte-for-byte** from pinned tzdb 2026a.

**What a declarative escape hatch would have needed** (spec, from this
concrete case):

- *Fetch step:* `url` + `sha256` + extract, like a dependency source but
  surfaced as a build input directory rather than a CMake package.
  Strawman: `[assets.tzdb] url = "https://data.iana.org/...tar.lz",
  sha256 = "...", extract = true` → path exposed to gen steps/tests.
  (The IANA tarball is `.tar.lz` — lzip; extraction must go through
  libarchive semantics like `file(ARCHIVE_EXTRACT)`/`cmake -E tar`,
  plain macOS `tar` also copes but that's luck.)
- *Generate step:* command with declared inputs (asset dir + template
  `known_zones.h.in`), declared outputs, and a re-run rule =
  input-hash change only (pure function of inputs → hermetic, cacheable
  in the store like a dep). Outputs land in a build-owned gen dir that
  can be added to a target's `includes`; vtz writes generated headers
  INTO THE SOURCE TREE (`include/impl/vtz/`), which a hermetic tool
  should refuse — checked-in-generated-output is the pattern to bless
  instead (a `verify` mode diffing regenerated output would cover it).
- *Runtime-data step:* the zic-compiled `data/zoneinfo` and extracted
  `data/tzdata` are needed only when *running* tests, not for any
  compile. That's a test-fixture provisioning hook (see testing-story),
  not a compile-graph node — conflating the two (as CMake does with the
  custom command wired via `add_dependencies(test_vtz compile_tzdata)`)
  is what makes the upstream flow non-hermetic (configure-time network
  I/O on every fresh build dir, unpinned cldr main URL, silent WARNING
  fallback when offline).

## 2. object-libraries — vtz_objects compiled twice (major)

**Upstream:** OBJECT lib `vtz_objects` (9 .cpp) is wrapped by `vtz`
(public headers = `include/api` only) and `vtz_impl` (internals exposed
for tests/bench). One compilation, two link-level facades with different
*interface* (include dirs/visibility), identical object code.

**cpp-pkg:** no object-library target kind, and no way to give one set
of objects two different public interfaces. Workaround: two
`static-library` targets compiling the same 9 sources twice. Costs:
double compile time, and a latent ODR trap — `test_vtz` links
`vtz_testing → date/GTest` plus `vtz_extras → vtz_impl`, while
`test_vtz_api` links `vtz_testing` plus `vtz` — if any target ever pulled
in *both* `vtz` and `vtz_impl` there would be duplicate symbols from two
copies of the same TUs (static libs: first-archive-wins, silent). The
real fix need not be OBJECT libraries per se: "same target, multiple
exported interface views" (a la Bazel's implementation_deps or a
`[targets.x.interfaces.impl]` sub-table) would model vtz/vtz_impl more
faithfully than CMake's own OBJECT-lib idiom does.

## 3. schema-ergonomics — dependency key IS the find_package name (major)

The probe runs `find_package(<depkey>)`, so the key must equal the CMake
package name: `abseil` failed (`abseilConfig.cmake` not found — package
is `absl`), and `googletest` would fail the same way (package `GTest`).
Error surfaced as a raw CMake probe-configure failure, not a hint. Keys
were renamed to `absl`/`GTest`. Cargo solves the analogous problem with
`package = "..."` renames; cpp-pkg needs a `cmake-package` (or
`find-package-name`) field so the key stays the user's chosen handle, and
the probe error should suggest it ("Config for '<key>' not found; if the
package's CMake name differs, set cmake-package").

## 4. per-target-flags (several instances)

- **Per-source compile definitions** (minor here, blocker in general):
  upstream sets `VTZ_EMBED_TZDB=$<BOOL:...>` on *one file*
  (`embedded_tzdb.cpp` via `set_source_files_properties`). Workaround:
  target-wide private define — safe only because exactly one TU tests
  the macro (verified by grep). The bench does it again with `-U/-D`
  per-source flags on `bench_date_with_os_tzdb.cpp`; there it is a
  blocker (see §6).
- **Target-scoped warnings** (minor): upstream adds `-Wall -Wextra` to
  `vtz_objects` only, dev warnings globally. cpp-pkg flags live only in
  profiles → applied to *every* project target. A `cxx-flags` (or
  `warnings`) field per target is the missing piece.
- **Profile-independent project flags** (minor): the warning/visibility
  set had to be pasted into `[profiles.release]` AND `[profiles.debug]`.
  Wants a profile-agnostic `[flags]`/`[build] cxx-flags` layer that
  profiles refine.
- **Toolchain-conditional flags** (minor): `-Wshorten-64-to-32` is
  clang-only (upstream guards with `if(CMAKE_CXX_COMPILER_ID MATCHES
  "Clang")`). No conditional in the manifest; fine on this machine,
  wrong under `--toolchain gcc-homebrew`.
- **Global PIC / visibility presets** (minor): `CMAKE_POSITION_INDEPENDENT_CODE
  ON` + hidden-visibility presets are ABI-relevant target properties in
  CMake; in cpp-pkg they're just profile flag strings. Dropped PIC
  (macOS arm64 all-PIC, static-only build) — on Linux with
  `VTZ_BUILD_SHARED=ON` this becomes real.

## 5. conditional-sources (major)

Three shapes in one project:

- `VTZ_BUILD_SHARED` switches `vtz` between STATIC and SHARED **and**
  gates the `VTZ_STATIC_DEFINE` public define. cpp-pkg has no
  shared-library type and no option system; the migration hard-commits
  to the static flavor. (Also: no `[options]`/feature system at all —
  `VTZ_ONLY`, `VTZ_EMBED_TZDB`, `VTZ_DATE_COMPAT` etc. all become
  editions of the manifest.)
- Glob-with-exclusion: bench sources = `glob(src/*.cpp)` minus
  `bench_date_with_os_tzdb.cpp` when the option is off. No exclusion
  syntax in cpp-pkg globs → explicit 7-file list that will silently
  miss future bench files. `sources = { glob = [...], exclude = [...] }`
  would cover it.
- Platform conditionals (WIN32/UNIX/MSVC) throughout upstream — moot on
  one machine, unrepresentable in the schema.

## 6. dep-provisioning — reaching into a dependency's source tree (blocker for the OS-tzdb bench)

`VTZ_ALSO_BENCH_HINNANT_OS_TZDB=ON` (the upstream *default* on unix)
builds `date_os_tzdb_tz` from `${date_SOURCE_DIR}/src/tz.cpp` — a file
of the **date dependency's source tree** — with per-source defines
renaming `namespace date` → `date_os_tzdb`, then links both date
variants into `bench_vtz`. cpp-pkg deps are opaque store artifacts;
there is no `${dep_SOURCE_DIR}` equivalent, no way to compile a dep's
file under different flags. Scope reduction: built the OFF
configuration (supported upstream; parity build also OFF). This is
admittedly an exotic pattern (CPM/FetchContent-era source access); a
package manager may reasonably never support it — but the gap should be
acknowledged as "source-visible deps" being a real CPM idiom.

## 6b. dep-provisioning — no way to patch a dependency (major)

abseil 20260107.1 (the exact version upstream vtz pins via CPM) has a
broken install export: `heterogeneous_lookup_testing` is missing its
`TESTONLY` marker (fixed later on absl master), so it is installed into
`abslTargets.cmake` while its link interface references
`absl::test_instance_tracker` (internal testonly, never installed) and
`GTest::gmock` (external). `find_package(absl)` on the installed prefix
hard-fails while loading the Config — which is exactly what cpp-pkg's
probe does. Upstream never noticed because CPM = `add_subdirectory`:
cpp-pkg's install-then-probe pipeline holds dependencies to a *stricter*
standard than the FetchContent ecosystem does, so it will keep tripping
over real-world packaging bugs like this one — and when it does, the
user needs an escape: **there is no `patches = [...]` field on a
dependency** (Conan/Nix/Buildroot all grew one for this exact reason).
Workaround: pin.sh stands up a *local clone* of abseil with the one-line
backport as a deterministic commit (pinned author/date → stable sha
`4645a01a...`) and the manifest consumes it as
`git = "file:///...deps/absl-patched"` + `rev` — which also forced a
placeholder-substitution step (`@ABSL_PATCHED_REPO@`) because file://
URLs are machine-specific, so the checked-in CppPkg.toml is no longer
literally buildable. A first-class `patches` field (hashed into the
config hash) would eliminate the whole contraption. Secondary finding:
`git = "file://..."` + `rev` works today (undocumented but handy).

## 6c. dep-provisioning — headers in a dep's INTERFACE_SOURCES are compiled (major, cpp-pkg bug)

Hinnant date v3.0.4 exports its public headers through
`target_sources(date INTERFACE .../date.h ...)` / `target_sources(date-tz
PUBLIC .../tz.h ...)` — explicitly commented "adding header sources just
helps IDEs". The installed `dateTargets.cmake` therefore carries
`INTERFACE_SOURCES ".../include/date/tz.h"`. CMake never compiles `.h`
entries; cpp-pkg's extractor turns interface sources into compilable
source units and hard-errors at plan time:
`source '.../include/date/tz.h' has unknown extension '.h'`.
Fix in cpp-pkg: skip header/unknown extensions in INTERFACE_SOURCES
(match CMake's own is-compilable classification) instead of erroring —
the strict extension table (right for *project* sources) is wrong for
*extracted* interface sources. Workaround: second locally-patched dep
clone (patches/deps-date-0001-*.patch) removing the IDE-decoration
exports. This idiom is common (spdlog, nlohmann do similar things with
header listings), so it will recur.

## 6d. dep-provisioning — CMake-builtin pseudo-packages (Threads) (minor)

Four dep configs (`GTest`, `absl`, `benchmark`, `date`) each call
`find_dependency(Threads)`, so `Threads::Threads` appears in four
extracted manifests → `error: ambiguous target reference
'Threads::Threads' (required by component 'date::date-tz' ...)`. The
error message is genuinely good (lists candidates + the exact
`exposes-*` fix), but the model is wrong for this case: nobody *owns*
Threads — it's a CMake find-module pseudo-package that every config
re-creates identically. Workaround per the error's own suggestion:
`exposes-targets = ["Threads::Threads"]` on the `date` entry
(arbitrary owner). cpp-pkg should special-case well-known module
pseudo-packages (Threads, and eventually OpenSSL::/ZLIB:: shapes when
they come from find_dependency of modules) as shared/unowned, deduping
identical definitions instead of demanding an owner.

## 6e. dep-provisioning — unevaluated generator expressions in LINK_ONLY (minor)

Extracting absl emits six notes like `absl::base: unhandled generator
expression inside LINK_ONLY: '$<$<BOOL:LIBRT-NOTFOUND>:-lrt>'` /
`'$<$<BOOL:>:-ladvapi32>'`. All of them evaluate to *false* on this
platform (dropping them is correct here), and surfacing a note instead
of silently eating them is the right instinct — but
`$<$<BOOL:LIBRT-NOTFOUND>:-lrt>` is textbook `$<BOOL:>` evaluation and
should be evaluated, not skipped: on Linux, `-lrt`-style entries behind
`$<BOOL:...>` guards that are TRUE would be silently dropped and links
would fail. The probe's genex evaluator handles `SHELL:` groups but not
conditional genexes inside LINK_ONLY.

## 7. testing-story (major)

- No test runner: upstream registers 774 ctest cases via
  `gtest_discover_tests` (three invocations with different
  prefixes/args, incl. an `--no_set_install` embedded-tzdb variant) and
  `add_standalone_test` (env-var matrix + `WILL_FAIL` death tests +
  `ENVIRONMENT_MODIFICATION` unset-var semantics). cpp-pkg has no
  `cpp-pkg test`; all invocations are hand-copied into README/run
  script. The death tests (expect nonzero exit) and env-var
  set/unset matrix are the parts a `[tests]` schema would actually have
  to express — args, cwd, env, expected-failure.
- No test-only deps: GTest/date/benchmark/absl build for every consumer
  of the manifest. Cargo's `[dev-dependencies]` split is the obvious
  shape; it also interacts with §1 (test fixture data provisioning).
- Working directory and fixture paths: upstream runs tests with
  `WORKING_DIRECTORY ${BUILD}` and passes `--build`/`--testdata` paths.
  The migration reuses that CLI, cwd `tzdb-runtime/`.

## 8. install-export (major, unexercised)

Upstream installs `vtz` with `FILE_SET HEADERS` (BASE_DIRS
`include/api` → clean header staging), an EXPORT with namespace
`vtz::`, and a `configure_package_config_file` vtzConfig.cmake;
`etc/date_util_example` is a standalone consumer via `find_package(vtz)`.
cpp-pkg v0 can only *consume*; it cannot produce an installed package
from `[targets]`, so the migration simply has no analog of
`install(...)`/the date_util_example consumer flow (not attempted —
nothing to attempt). Note the FILE_SET pattern is exactly the metadata a
`[targets.vtz] public-headers = { base = "include/api", glob = "**/*.h" }`
field would need; vtz's `include/api`-vs-`include/impl` split maps to it
perfectly.

## 9. Non-gaps worth recording (things that worked)

- ALIAS-target invisibility (known limitation) did NOT bite: all
  consumed names (`unordered_dense::unordered_dense`, `date::date`,
  `date::date-tz`, `GTest::gtest`, `GTest::gmock`, `fmt::fmt`,
  `benchmark::benchmark`, `absl::time`, `absl::time_zone`) are real
  IMPORTED targets in their installed Configs. The aliases upstream
  defines (`vtz::vtz` etc.) are *project-local* and simply become plain
  target names.
- Replacing the inline-vendored ankerl with the real
  `martinus/unordered_dense@v4.8.1` package was a drop-in (same
  version as the vendored header; INTERFACE imported target extracted
  fine).
- Hinnant date consumed with the same options upstream passes CPM
  (`BUILD_TZ_LIB=ON`, `MANUAL_TZ_DB=ON`) — option pass-through is the
  right shape.
- The quoted define
  `VTZ_TZDATA_PATH_VARS="VTZ_TZDATA_PATH"` survived TOML → ninja →
  shell quoting intact (`-Werror=uninitialized` and the full warning set
  also passed over the whole tree, so the flags really applied).
- `git = "file:///..."` + `rev` dependencies work (used for both patched
  dep clones); lockfile entries record them faithfully.
- Store caching: second `cpp-pkg build` = 0 dependency builds, `ninja:
  no work to do`, 0.08 s wall.
- The naming ladder needed zero `exposes-*` for the ordinary case:
  `date::date-tz`, `GTest::gmock`, `absl::time_zone`,
  `benchmark::benchmark`, `unordered_dense::unordered_dense`, `fmt::fmt`
  all resolved by uniqueness (step 1).

---

## Parity evidence (2026-08-14, macOS arm64, Apple clang 21, Release)

Upstream CMake build (fresh dir, `-DCMAKE_BUILD_TYPE=Release
-DVTZ_ALSO_BENCH_HINNANT_OS_TZDB=OFF`, CMake 4.4 + Ninja):

- configure/build clean; `ctest`: **774/774 passed** (prefixes: impl/ 50,
  api/ 360, api_embed/ 360, standalone api/test_tzdb_load 4).

cpp-pkg build (this manifest, same checkout, `CPPKG_STORE` fresh):

- all 15 targets build and link; same gtest binaries run manually with
  upstream's exact args/cwd (see README):
  - `test_vtz --build . --testdata ../etc/testdata` → **50/50 PASSED**
  - `test_vtz_api` (same args) → **360/360 PASSED**
  - `test_vtz_api --no_set_install` (embedded-tzdb mode) → **360/360
    PASSED**
  - standalone matrix: set_install OK, env-var OK, both death tests
    exit nonzero as required → 4/4
  - total **774/774**, matching ctest.
- `vtz_tldr` output diffed **byte-identical** against the CMake-built
  binary's output (timezone conversions, offsets, zone-info dump).
- `bench_vtz` runs (`locate_zone/date 22.9ns, /absl 16.5ns, /vtz
  6.6ns`) — same relative ordering as upstream's published claim (vtz
  fastest).
- tzdata acquisition: pinned tzdb-2026a
  (sha256 0913509a…) reproduces upstream's configure-time download;
  `--verify-refresh` regenerated `embedded_tzdb_content.h` and
  `known_zones.h` from tzdata.zi **byte-identical** to the checked-in
  files.
- second cpp-pkg build: full store cache hit (0 dep builds, ninja
  no-op).

Deviations from upstream scope (all documented above):
`bench_date_with_os_tzdb.cpp` excluded (= the
`VTZ_ALSO_BENCH_HINNANT_OS_TZDB=OFF` config, §6); absl + date consumed
via locally-patched clones (§6b, §6c); install/export of vtz itself not
attempted (§8); shared-library flavor not attempted (§5).
