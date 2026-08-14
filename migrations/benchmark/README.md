# Migration: google/benchmark v1.9.5

- Upstream: https://github.com/google/benchmark
- Ref: tag `v1.9.5` = commit `192ef10025eb2c4cdd392bc502f0c852196baa48`
- Machine: macOS arm64, AppleClang 21, cmake 4.4, ninja
- Status: **green** in both modes, zero patches, several documented gaps
  (`GAPS.md`).

Google Benchmark is a microbenchmark library: two static libraries
(`benchmark`, `benchmark_main`), a heavily configure-time-driven CMake build
(git-describe version stamping, `cxx_feature_check` try_run probes, warning
flag probing via `add_cxx_compiler_flag`), a gtest-based test suite, and a
full install/export story (headers, `benchmarkConfig.cmake`, pkg-config
files, python tools).

## What was migrated

Two modes, both exercised:

1. **Build-from-source** (`CppPkg.toml`, staged into the upstream root by
   `pin.sh`): targets `benchmark` (19 explicit `.cc` files — upstream's
   glob-minus-benchmark_main.cc), `benchmark_main`, and `basic-bench`
   (upstream's own `test/basic_test.cc`, which ends in `BENCHMARK_MAIN()` and
   needs no gtest) as the proof executable.
2. **Declare-as-dependency** (`consumer/`): a `bench-demo` executable
   consuming `benchmark::benchmark_main` via the normal cpp-pkg pipeline
   (git dep → CMake build into store → tier-2 probe of the installed
   `benchmarkConfig.cmake`). The installed `benchmark::*` targets are real
   IMPORTED targets (not ALIASes, unlike curl), so the probe sees them
   directly; `find_dependency(Threads)` resolves via CMake's find-module and
   needed no `needs` entry.

## Scope reductions (explicit)

- The gtest test suite (~40 CTest tests) is **not** migrated: cpp-pkg has no
  test targets, test-only dependencies, or runner (GAPS: testing-story).
  Equivalent of `BENCHMARK_ENABLE_TESTING=OFF`.
- The install/export payload (headers, config files, `.pc` files, tools) is
  **not** produced by the source-mode build: cpp-pkg has no install story
  (GAPS: install-export).
- The manifest encodes the macOS/arm64/AppleClang configure outcome only
  (feature-probe defines, no `rt`/`shlwapi`/`kstat`); it is not portable
  (GAPS: conditional-sources, codegen-escape-hatch).

## Reproduce

```sh
cd migrations/benchmark
./pin.sh
cd upstream && CPPKG_STORE=/tmp/store cpp-pkg build && ./build/basic-bench
cd ../consumer && CPPKG_STORE=/tmp/store cpp-pkg build && ./build/bench-demo
```

## Parity protocol and evidence

Reference build: fresh `cmake -G Ninja -DCMAKE_BUILD_TYPE=Release
-DBENCHMARK_ENABLE_TESTING=OFF` + install of the same checkout.

- **Object files bit-identical**: `cmp` of `benchmark.cc.o` and
  `sysinfo.cc.o` from the CMake build vs the cpp-pkg build — identical bytes.
  Archives (`libbenchmark.a`, `libbenchmark_main.a`) have identical sizes
  (598176 / matching) and identical `nm -g` symbol lists (625 exported
  symbols); the archives differ only in `ar` member metadata.
- **Flags matched by construction**: profile `cxx-flags` reproduce the
  reference `build.ninja` FLAGS line exactly (same warning battery,
  `-fvisibility=hidden -fvisibility-inlines-hidden`, `-O3 -DNDEBUG
  -std=c++17`), plus one added suppression (`-Wno-unused-but-set-variable`)
  needed because flags cannot be scoped to the test TU (documented deviation;
  suppression only, object code unchanged — proven by the bit-identical
  objects, which don't include the test TU).
- **Version string**: `BENCHMARK_VERSION="v1.9.5"` hardcoded; embedded string
  in `benchmark.cc.o` and `--benchmark_format=json` `library_version` both
  report `v1.9.5`, matching the reference (which got it via `git describe`).
- **Runtime**: `basic-bench` runs and prints timings (e.g. `BM_empty
  0.341 ns`); consumer `bench-demo` built via cpp-pkg vs the same source
  built via plain CMake `find_package(benchmark)` against the CMake install:
  `--benchmark_list_tests` output identical, timings within noise, both
  report `library_version: v1.9.5`.
- **Store determinism**: second consumer build is a full store cache hit (no
  dependency rebuild, `ninja: no work to do`); source-mode rebuild is a
  no-op.

## Files

- `CppPkg.toml` — source-mode manifest (source of truth; `pin.sh` stages it)
- `consumer/CppPkg.toml`, `consumer/src/main.cc`, `consumer/CppPkg.lock` —
  dependency-mode consumer
- `pin.sh` — pins the upstream checkout
- `patches/` — empty by design (no source edits needed)
- `GAPS.md` — friction points keyed to the design questions
