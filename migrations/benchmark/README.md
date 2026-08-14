# Migration: google/benchmark v1.9.5 — wave-2 edition

- Upstream: https://github.com/google/benchmark
- Ref: tag `v1.9.5` = commit `192ef10025eb2c4cdd392bc502f0c852196baa48`
- Machine: macOS arm64, AppleClang 21, cmake 4.4, ninja
- Status: **green** in both modes, zero patches, **full test suite ported**
  (84/84 invocations pass, matching the reference CTest run 1:1). Dissolved
  vs. remaining gaps: `GAPS.md`.

Google Benchmark is a microbenchmark library: two static libraries
(`benchmark`, `benchmark_main`), a heavily configure-time-driven CMake build
(git-describe version stamping, `cxx_feature_check` try_run probes, warning
flag probing via `add_cxx_compiler_flag`), a gtest-based test suite, and a
full install/export story. Wave 1 ported the two libraries as a macOS-only
projection and cut the suite and the install story; wave 2 restores all of
it as real schema surface.

## What was migrated

1. **Build-from-source** (`CppPkg.toml` + `CppPkg.lock`, staged into the
   upstream root by `pin.sh`):
   - `benchmark` / `benchmark_main` (`install = true` via `[target-defaults]`
     eligibility — dev/test targets skip it automatically);
   - sources as upstream's own shape: `["src/*.cc", "!src/benchmark_main.cc"]`;
   - warning battery in `[flags]` with the clang-only members
     (`-Wshorten-64-to-32`, `-Wthread-safety`) under `[flags.cfg.clang]`;
   - probe results as `cfg` transcriptions (`# transcribed:` comments):
     Linux branch (`BENCHMARK_HAS_PTHREAD_AFFINITY`, `-lrt`) and Windows
     branch (`shlwapi.lib`) written from upstream's build logic, validated
     but inactive here (S5 exercises Linux);
   - `BENCHMARK_VERSION="v${package.version}"` interpolated define;
   - `Threads::Threads` builtin edge (upstream's PRIVATE link, dropped in
     wave 1);
   - **the whole test suite**: googletest as `[dev-dependencies]` (upstream's
     own bundled pin v1.15.2, `find-package = "GTest"`), `output_test_helper`
     as `dev = true` library, 49 `test = true` executables, 84 `[[run]]`
     entries transcribed from upstream's CTest registrations (36 of them the
     `filter_test` matrix), per-target overrides (`donotoptimize_test`
     `-O3 -Werror=deprecated-declarations`, `cxx11_test` `cxx-std = 11`).
2. **Declare-as-dependency** (`consumer/`): unchanged manifest; `bench-demo`
   consumes `benchmark::benchmark_main` via git dep → CMake store build →
   probe of the installed `benchmarkConfig.cmake`.

## Scope reductions (explicit)

- `.pc` files (`benchmark.pc`, `benchmark_main.pc`), python tools
  (`share/googlebenchmark/tools`) and docs installs: out of scope for
  `cpp-pkg install` (recorded loss, spec'd as such).
- Assembly tests: upstream itself gates them off here (x86_64 + FileCheck
  required; this machine is arm64).
- Solaris `kstat`: out of the cfg vocabulary (comment in the manifest).
- The test stanzas transcribe upstream's **non-Debug** branch (`-UNDEBUG` +
  `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS`); a debug-config test build
  differs from upstream there (profile-conditional defines are not
  expressible — see GAPS).

## Reproduce

```sh
cd migrations/benchmark
./pin.sh
cd upstream
CPPKG_STORE=/tmp/store cpp-pkg build          # the two libraries only
CPPKG_STORE=/tmp/store cpp-pkg test           # fetches gtest (dev-dep, lazy),
                                              # builds 49 test targets, runs
                                              # 84 invocations
CPPKG_STORE=/tmp/store cpp-pkg install --prefix /tmp/prefix
cd ../consumer && CPPKG_STORE=/tmp/store cpp-pkg build && ./build/bench-demo
```

## Parity protocol and evidence

Reference build: fresh `cmake -G Ninja -DCMAKE_BUILD_TYPE=Release
-DBENCHMARK_DOWNLOAD_DEPENDENCIES=ON` (testing **ON** this wave — the only
library-flag difference vs. wave 1's testing-OFF reference is upstream adding
`-Wsuggest-override` when testing is off, which this manifest therefore
omits).

- **Object files bit-identical**: `cmp` of `benchmark.cc.o`, `sysinfo.cc.o`,
  `benchmark_register.cc.o`, `timers.cc.o`, `benchmark_main.cc.o` from the
  reference vs. the cpp-pkg build — identical bytes, although the manifest
  now splits the battery across `[flags]`/`[flags.cfg.clang]` (flag order
  differs; codegen doesn't). `benchmark.cc.o` embeds the interpolated
  version string, so `${package.version}` reproduced `git describe` exactly.
- **Symbol lists**: `nm -g` of `libbenchmark.a` identical (811 names).
- **Test parity**: reference `ctest` = 84 tests, 100% pass; `cpp-pkg test` =
  the same 84 invocations (names and argv transcribed 1:1), 84 passed / 0
  failed. `cpp-pkg test filter_test` runs the 36-entry matrix alone; a
  non-matching filter hard-errors listing all 49 targets.
- **Install**: 7 files staged (2 archives, 2 headers, Config +
  SameMajorVersion ConfigVersion + cppkg-manifest.json). **Fixpoint test
  from wave-1 GAPS now passes**: a plain CMake project's
  `find_package(benchmark 1.9.5 REQUIRED)` against the cpp-pkg-installed
  prefix configures, links `benchmark::benchmark_main`, runs, and reports
  `library_version: v1.9.5`.
- **Store determinism**: second `cpp-pkg build` and `cpp-pkg test` are
  no-ops (`ninja: no work to do`, no store work); second consumer build is a
  full cache hit.
- **Laziness**: `cpp-pkg build` of the libraries locks googletest eagerly
  (it is in `CppPkg.lock`) but never fetches or builds it; only
  `cpp-pkg test` provisions it.

## Files

- `CppPkg.toml` — source-mode manifest (source of truth; `pin.sh` stages it)
- `CppPkg.lock` — lockfile incl. the googletest dev-dep pin (staged too)
- `consumer/CppPkg.toml`, `consumer/src/main.cc`, `consumer/CppPkg.lock` —
  dependency-mode consumer
- `pin.sh` — pins the upstream checkout, stages manifest + lock
- `patches/` — empty by design (no source edits needed, either wave)
- `GAPS.md` — wave-2 edition: dissolved workarounds and what remains
