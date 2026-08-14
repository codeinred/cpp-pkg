# Migration: googletest v1.18.0 — wave-2 edition

Upstream: https://github.com/google/googletest
Ref: tag `v1.18.0` = commit `063de7e9578f82b369302001269680b4b1553359`
Machine: macOS arm64, AppleClang 21, cmake 4.4, ninja. Status: **green**.

## What this project is

GoogleTest — gtest, gtest_main, gmock, gmock_main as static libraries, plus
10 sample test executables (`googletest/samples`). The community CMake build
compiles each library from a single umbrella TU (`src/gtest-all.cc`,
`src/gmock-all.cc`) and installs a `GTest::` package (`GTestConfig.cmake`).

## Layout

- `CppPkg.toml` — the migrated build (14 targets). Source of truth; `pin.sh`
  copies it into the checkout root.
- `pin.sh` — clones upstream at the pinned commit into `./upstream` and
  stages `CppPkg.toml`. No patches/ — upstream sources build unmodified.
- `consumer/` — the ecosystem test: a separate project consuming googletest
  as a `[dev-dependencies]` GIT dep (tier-2 probe of the installed
  `GTestConfig.cmake`), one gtest+gmock test target run via `cpp-pkg test`.
- `GAPS.md` — wave-2 edition: what the wave-1 extensions dissolved, what
  remains.

## What changed from the wave-1 port

- **Samples are `test = true`**: they leave the default build (`cpp-pkg
  build` = the 4 libraries, 8 ninja steps) and run under `cpp-pkg test`
  (10/10 pass; the shell for-loop is gone). No `[[run]]` entries needed —
  default invocations are the suite.
- **`[target-defaults]`**: `cxx-std = 17` written once (upstream's single
  PUBLIC `cxx_std_17`), `install = true` (fills the 4 libraries, skips the
  test targets automatically), and the `GTEST_HAS_PTHREAD=1` private define.
- **Upstream's warning sets are back**, transcribed from
  `internal_utils.cmake`: `cxx_base_flags` under `[flags.cfg.clang]` /
  `[flags.cfg.gcc]` (every target, like upstream's `cxx_default`),
  `cxx_strict_flags` as private per-target `cxx-flags` under
  `cfg.clang`/`cfg.gcc` on the 4 libraries only. Note the deliberate
  divergence: cpp-pkg's `clang` matches AppleClang, upstream's
  `STREQUAL "Clang"` does not — so on this machine cpp-pkg compiles with the
  full strict set (0 warnings emitted, verified) while upstream's own build
  compiles warning-free by accident. Same artifacts. The gcc branches are
  written for the Linux validation stage.
- **`system-includes = true`** on the libraries: consumers get the gtest
  dirs as `-isystem`, matching upstream's `SYSTEM INTERFACE` (verified in
  sample compile lines).
- **`Threads::Threads`** is a public dep of each library (upstream links it
  PUBLIC on every `cxx_library`): builtin, no declaration — `-pthread` on
  linux, nothing on macos, and the emitted Config carries
  `find_dependency(Threads)` like upstream's.
- **`[export] cmake-name = "GTest", namespace = "GTest"`** + `cpp-pkg
  install`: adopting CppPkg.toml no longer orphans the
  `find_package(GTest)` ecosystem. `public-headers` overrides pin the
  installed set to `include/gtest` + `include/gmock` (byte-identical to the
  upstream include trees) even though the source dirs are public
  build-interface includes.
- **Consumer**: googletest moved to `[dev-dependencies]` (a downstream
  consumer of this package never resolves it; `cpp-pkg build` does zero
  store work), `calc_test` is `test = true` with a `[[run]]` entry passing
  `--gtest_brief=1`.

## Reproduce

```sh
cd migrations/googletest
./pin.sh                                   # clones ./upstream, stages CppPkg.toml
export CPPKG_STORE=/tmp/store-s4-googletest

( cd upstream && cpp-pkg build )           # 4 libraries only
( cd upstream && cpp-pkg test )            # builds + runs the 10 samples
( cd upstream && cpp-pkg test sample7_unittest -- --gtest_list_tests )  # filter + passthrough
( cd upstream && cpp-pkg install --prefix /tmp/gtest-prefix )           # GTest package

( cd consumer && cpp-pkg build )           # no-op: dev-dep untouched
( cd consumer && cpp-pkg test )            # fetches/builds googletest lazily, runs calc_test
```

Fixpoint check (raw CMake against our emission):

```sh
cmake -S cmake-consumer -B build -DCMAKE_PREFIX_PATH=/tmp/gtest-prefix   # find_package(GTest 1.18 CONFIG)
```

## Parity evidence (2026-08-14, wave 2)

- **`cpp-pkg test`**: 10 passed, 0 failed (10 invocations across 10 test
  targets). Per-binary outcomes identical to wave 1 and the CMake
  reference: sample1 6/6, sample2 4/4, sample3 3/3, sample4 1/1, sample5
  4/4, sample6 12/12, sample7 6/6, sample8 12/12, sample9 2 pass + 1
  intentional failure (rc=0 by upstream design), sample10 2/2.
- **Archives**: member lists identical to upstream (umbrella object per
  lib, e.g. `libgtest.a` = `gtest-all.cc.o`).
- **Compile line** for `gtest-all.cc` is upstream's `cxx_strict` verbatim:
  `-std=c++17 -O3 -DNDEBUG -Wall -Wshadow -Wconversion -Wundef
  -fexceptions -W -Wpointer-arith -Wreturn-type -Wcast-qual
  -Wwrite-strings -Wswitch -Wunused-parameter -Wcast-align -Winline
  -Wredundant-decls -Wchar-subscripts -I<include> -I<srcdir>
  -DGTEST_HAS_PTHREAD=1`. Samples get `cxx_default` (base flags) with the
  library dirs as `-isystem`.
- **Install/export**: 45 files — `lib/*.a` ×4, 38 headers byte-identical
  in set to upstream's `include/` trees, `GTestConfig.cmake` +
  `GTestConfigVersion.cmake` + `cppkg-manifest.json` under
  `lib/cmake/GTest/`. No `bin/` (test targets excluded from install). A raw
  CMake consumer (`find_package(GTest 1.18 CONFIG REQUIRED)`, links
  `GTest::gmock` + `GTest::gtest_main`) configures, builds, and passes.
- **Store cache hit**: consumer `rm -rf build && cpp-pkg test` = 0.84 s
  total (dep is a pure store hit); in-tree re-`test` = 0.06 s, no rebuild.
