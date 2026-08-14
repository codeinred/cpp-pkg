# Migration: vtz (voladynamics/vtz) → cpp-pkg

## What the project is

`vtz` is a C++17 IANA-timezone library (the user's own project). Upstream
build: CMake 3.23. Notable machinery:

- **OBJECT library** `vtz_objects` (all of `include/api/*.cpp` +
  `include/impl/*.cpp`) wrapped by two real libraries: `vtz`
  (static/shared switch via `VTZ_BUILD_SHARED`, public headers =
  `include/api` only, installed with `FILE_SET HEADERS`) and `vtz_impl`
  (same objects, internals in `include/impl` exposed for tests/bench).
- **ALIAS targets** `vtz::vtz`, `vtz::impl`, `vtz::extras`, `vtz::testing`.
- **Inline-vendored** `ankerl::unordered_dense` 4.8.1 (`etc/3rd/ankerl`) as
  an INTERFACE lib + `unordered_dense::unordered_dense` ALIAS.
- **Configure-time tzdb acquisition**: `file(DOWNLOAD)` of
  `tzdb-2026a.tar.lz` from IANA + `file(ARCHIVE_EXTRACT)` +
  `file(CREATE_LINK)`; `zic` custom command compiling `data/zoneinfo`;
  `windowsZones.xml` downloaded from cldr **main** (unpinned). Under
  `VTZ_REFRESH_TZDATA` it also regenerates
  `include/impl/vtz/embedded_tzdb_content.h` (tzdata.zi as C string chunks
  via `file(WRITE)`) and `include/impl/vtz/known_zones.h`
  (`configure_file` from `Z `/`L ` lines of tzdata.zi). Both generated
  headers are **checked in**, so a normal build only needs the
  download/extract/zic part — and only for *tests*, not for the library.
- Tests (`etc/test`): `vtz_testing` static lib + `test_vtz`,
  `test_vtz_api`, standalone `test_tzdb_load` (death tests, env-var
  matrix) via `gtest_discover_tests`/`add_standalone_test`; reference
  implementation Hinnant `date` (CPM, `MANUAL_TZ_DB=ON`), googletest.
- Bench (`etc/bench`): `bench_vtz` vs abseil + Hinnant date; optional
  second copy of date's tz.cpp compiled with `-Ddate=date_os_tzdb`
  straight out of the dep's source dir (`VTZ_ALSO_BENCH_HINNANT_OS_TZDB`).
- Examples/tools: one-exe-per-file `foreach(glob)` loops.
- Global flags: dev warnings, `CMAKE_POSITION_INDEPENDENT_CODE ON`,
  hidden visibility preset.

## Pin

- url: https://github.com/voladynamics/vtz
- ref: `main` (no release tag pinned per brief; tags v1.0.0/v1.1.0 exist)
- commit: `8d6ea8f35ed18fb72b9796a1d9a843df0529baf0`
- tzdb: 2026a, sha256 `0913509a37f26b81bb6396018ad5cdf32065374ed36e82cceb61b2ee57a94b7c`
- windowsZones.xml: cldr commit `eef56793a2616c8b9f2e5f62b01df2621f9a18d6`,
  sha256 `9cf3db6a31fb382fee21b70be6feba1e82766b0fcd06e6261fb7936a73e537ff`

## Migration approach

- `CppPkg.toml` (this dir, source of truth; `pin.sh` copies it into the
  checkout) declares: `vtz`, `vtz_impl`, `vtz_extras`, `vtz_testing`,
  `test_vtz`, `test_vtz_api`, `test_tzdb_load`, 4 example exes,
  `dump_tzfile`, `bench_vtz`.
- The OBJECT library is flattened: `vtz` and `vtz_impl` are two static
  libraries compiling the same sources twice (no object-library kind).
- Vendored ankerl replaced by a real dependency
  `martinus/unordered_dense@v4.8.1` (same version as the vendored header).
- Test/bench deps declared as ordinary dependencies: fmt 11.2.0,
  googletest 1.17.0, HowardHinnant/date 3.0.4 (`BUILD_TZ_LIB=ON`,
  `MANUAL_TZ_DB=ON`), benchmark 1.9.4, abseil 20260107.1 — same
  versions/options upstream passes to CPM.
- tzdb acquisition moved out of the build into
  `scripts/fetch-tzdata.sh` (invoked by `pin.sh`): pinned download +
  checksum, extract via `cmake -E tar` (same libarchive as
  `file(ARCHIVE_EXTRACT)`), symlink, pinned windowsZones.xml, `zic`. Its
  `--verify-refresh` mode re-derives both checked-in generated headers
  from tzdata.zi and diffs them (they reproduce byte-for-byte — so the
  `VTZ_REFRESH_TZDATA` codegen is fully specified by ~20 lines of
  python; see GAPS.md `codegen-escape-hatch`).
- `bench_date_with_os_tzdb.cpp` is excluded, i.e. we build the
  `VTZ_ALSO_BENCH_HINNANT_OS_TZDB=OFF` configuration (a supported
  upstream config; the ON variant needs per-source `-U/-D` flags and
  compiles a file out of the date dependency's *source tree* — see
  GAPS.md). Everything else is at full upstream scope.
- Upstream's global warning/visibility flags are expressed as profile
  `cxx-flags` (duplicated across release/debug; see GAPS.md).
  `CMAKE_POSITION_INDEPENDENT_CODE` is dropped: macOS arm64 is all-PIC
  and only the static `vtz` flavor is built (`VTZ_BUILD_SHARED=OFF`
  upstream default).

## Reproduce

```sh
./pin.sh /path/to/stage            # clone + stage + tzdata + codegen verify
cd /path/to/stage
CPPKG_STORE=/path/to/store /opt/claude/cpp-pkg/target/debug/cpp-pkg build

# tests (upstream runs these via ctest/gtest_discover_tests; cwd and args
# mirror etc/test/CMakeLists.txt):
cd tzdb-runtime
../build/test_vtz     --build . --testdata ../etc/testdata
../build/test_vtz_api --build . --testdata ../etc/testdata
../build/test_vtz_api --no_set_install --build . --testdata ../etc/testdata
../build/test_tzdb_load "$PWD/data/tzdata"                       # set_install path
VTZ_TZDATA_PATH="$PWD/data/tzdata" ../build/test_tzdb_load       # env path
VTZ_TZDATA_PATH=/bad/env/path ../build/test_tzdb_load; test $? -ne 0  # death test
../build/vtz_tldr                  # example
cd ..
ln -sfn ../tzdb-runtime/data build/data   # bench uses default path build/data/tzdata, cwd = project root
./build/bench_vtz --benchmark_filter=locate_zone  # bench smoke
```

Note: the staged CppPkg.toml differs from the checked-in one only in the
`@ABSL_PATCHED_REPO@`/`@DATE_PATCHED_REPO@` placeholders, which pin.sh
substitutes with `file://` URLs of two locally patched dependency clones
(abseil: broken install export at 20260107.1; date: headers exported via
INTERFACE_SOURCES trip a cpp-pkg extraction bug). See `patches/deps-*.patch`
and GAPS.md §6b/§6c.

## Parity protocol & results

See GAPS.md bottom section for the full evidence. Summary (all verified
2026-08-14):

- Upstream CMake (fresh dir, Release, `VTZ_ALSO_BENCH_HINNANT_OS_TZDB=OFF`):
  774/774 ctest tests pass.
- cpp-pkg build: same test binaries, run manually with upstream's exact
  args/cwd — 50 + 360 + 360 + 4 = 774/774 pass.
- `vtz_tldr` output byte-identical between CMake-built and cpp-pkg-built
  binaries; `bench_vtz` runs and reproduces the expected ranking.
- Checked-in generated headers (`embedded_tzdb_content.h`,
  `known_zones.h`) reproduce byte-for-byte from pinned tzdb 2026a
  (`fetch-tzdata.sh --verify-refresh`).
- Second `cpp-pkg build`: full store cache hit (0 dependency builds,
  `ninja: no work to do`).
