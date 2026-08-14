# Migration: googletest v1.18.0

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
  stages `CppPkg.toml`. **No patches/** — upstream sources build unmodified.
- `consumer/` — the ecosystem test: a separate project consuming googletest
  as a normal cpp-pkg GIT dependency (tier-2 probe of the installed
  `GTestConfig.cmake`), one gtest+gmock test executable.
- `GAPS.md` — friction points, keyed to the design questions.

## Migration decisions

- **Umbrella TUs, matching upstream**: `gtest` = `gtest-all.cc` only (not the
  11 individual sources) — this is exactly what upstream's non-MSVC
  `cxx_library()` path does, and it makes archive parity checkable
  (identical member lists, see below).
- **Include layout**: `googletest/include` *and* `googletest` are both public.
  The source dir is needed privately so `gtest-all.cc` can
  `#include "src/gtest.cc"`, but upstream also exports *both* dirs in its
  `INTERFACE` include dirs, so public matches the real interface.
- **`GTEST_HAS_PTHREAD=1`** as a private define on each library — upstream
  injects `-DGTEST_HAS_PTHREAD=1` via `COMPILE_FLAGS` (private) after
  `find_package(Threads)` succeeds. Consumers rely on `gtest-port.h`
  autodetection, same as with upstream's installed package.
- **`cxx-std = 17` repeated on all 14 targets** — upstream propagates
  `cxx_std_17` as a PUBLIC compile feature; cpp-pkg has no propagating
  equivalent (GAPS.md).
- Upstream's strict warning set for the libraries is not reproduced (no
  per-target flags in cpp-pkg). On this machine that is *zero* parity loss:
  upstream's compiler branches test `CMAKE_CXX_COMPILER_ID STREQUAL "Clang"`,
  which AppleClang fails, so the CMake build also compiles with no warning
  flags (verified in `build.ninja`: `FLAGS = -O3 -DNDEBUG -std=c++17 -arch
  arm64 -DGTEST_HAS_PTHREAD=1`).
- Samples are unconditional targets; upstream gates them behind
  `-Dgtest_build_samples=ON` (no option-gated targets in cpp-pkg, GAPS.md).

## Reproduce

```sh
cd migrations/googletest
./pin.sh                                   # clones ./upstream, stages CppPkg.toml
export CPPKG_STORE=/tmp/store-mig-googletest
( cd upstream && cpp-pkg build )           # 4 libs + 10 samples
for i in $(seq 1 10); do upstream/build/sample${i}_unittest; done
( cd consumer && cpp-pkg build && ./build/calc_test )   # ecosystem test
```

## Parity evidence (2026-08-14)

CMake reference: `cmake -S upstream -B cmake-build -G Ninja
-DCMAKE_BUILD_TYPE=Release -Dgtest_build_samples=ON && cmake --build
cmake-build`.

- **All 10 samples, both builds**: identical outcomes —
  sample1: 6/6 pass; sample2: 4/4; sample3: 3/3; sample4: 1/1; sample5: 4/4;
  sample6: 12/12; sample7: 6/6; sample8: 12/12; sample9: 2 pass + 1
  intentional failure (`CustomOutputTest.Fails`, rc=0 by design — custom
  listener demo); sample10: 2/2. Exit codes match (all 0).
- **Archives**: member lists identical for all four libs (e.g. `libgtest.a` =
  `gtest-all.cc.o` in both).
- **Compile command** for `gtest-all.cc`: flag-for-flag equivalent
  (cpp-pkg: `-std=c++17 -isysroot <SDK> -O3 -DNDEBUG -I.../include
  -I.../googletest -DGTEST_HAS_PTHREAD=1`; CMake adds an explicit
  `-arch arm64`, cpp-pkg uses the host default — same output arch).
- **Ecosystem consumer**: googletest fetched by tag, built+installed by its
  own CMake into the store, probed → manifest exports `GTest::gtest`,
  `GTest::gtest_main`, `GTest::gmock`, `GTest::gmock_main` (plus an absorbed
  `Threads::Threads`); `calc_test` (gtest_main + gmock mock/matchers) builds,
  links, passes 2/2.
- **Store cache hit**: `rm -rf consumer/build && cpp-pkg build` completes in
  0.96 s running only the consumer's 2 steps — the multi-minute googletest
  dep build is a pure store hit.
