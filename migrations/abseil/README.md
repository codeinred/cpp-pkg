# Migration: abseil-cpp (LTS 20260526.0) — wave-2 edition

Upstream: https://github.com/abseil/abseil-cpp
Ref: tag `20260526.0` = commit `5650e9cf76d3be4318d5fa3af38ee483ddfd5e4a`.

Abseil is the target-count ergonomics candidate: 256 `absl_cc_library` calls
(216 non-test) plus 241 `absl_cc_test` calls, built through a Bazel-like
CMake function layer. This is the S4 re-migration: every wave-1 workaround
the wave-1 extensions were designed to dissolve is now real syntax (see
GAPS.md "Dissolved"), and the port gained the two things wave 1 could not
express at all — a test suite and an installable export.

Two experiments:

1. **Native port** (`CppPkg.toml`): the transitive closure of
   `strings` + `str_format` + `flat_hash_map` (93 library targets, 54 of
   them header-only) + a `demo` executable, **plus** (new in wave 2) the
   part of upstream's gtest suite whose dependency closure lies inside that
   subset: 21 TESTONLY libraries (`dev = true`) and 88 `absl_cc_test`
   executables (`test = true`), mined 1:1 by `gen_toml.py`. GoogleTest is a
   `[dev-dependencies]` entry — a library consumer never builds it.
2. **Consumer** (`consumer/`): abseil as an ordinary dependency, exercising
   the tier-2 probe at scale (217 extracted components) — now against
   **unpatched upstream** (the 20260526.0 self-edge is deduped tool-side
   since wave 1; the wave-1 `file://` patched-clone machinery is deleted).

## Reproduce

```sh
cd <workdir>
sh /opt/claude/cpp-pkg/migrations/abseil/pin.sh   # clones + overlays
export CPPKG_STORE=<somewhere>

# native port: default build = 249 actions, tests excluded
( cd upstream && cpp-pkg build && ./build/demo )

# test suite: builds googletest (dev-dep, lazy) + 21 dev libs + 88 tests
( cd upstream && cpp-pkg test --jobs 8 )
# filtering + gtest passthrough:
( cd upstream && cpp-pkg test str_format_test -- --gtest_filter='FormatEntryPointTest.*' )

# install/export: FHS prefix with abslConfig.cmake — the port is a producer now
( cd upstream && cpp-pkg install --prefix "$PWD/../port-install" )

# consumer (dep = plain upstream, no patch)
( cd consumer && cpp-pkg build && ./build/demo )
```

`pin.sh` never edits upstream sources — it only ADDS `CppPkg.toml`,
`cppkg_stub.cc`, and `demo/main.cpp`.

Regenerating the manifest: `python3 gen_toml.py <upstream-root>` emits the
generated target blocks; `CppPkg.toml` = `header.toml` + that output.

## Files

- `CppPkg.toml` — native-port manifest (1548 lines; 203 targets: 93 libs +
  21 dev libs + 88 tests + demo). Everything below the marker line is
  generator output.
- `header.toml` — hand-written prologue: package, `[export]`
  (`cmake-name`/`namespace = "absl"`), upstream COPTS as
  `[flags.cfg.clang]`/`[flags.cfg.gcc]` (transcribed from
  `GENERATED_AbseilCopts.cmake`), `[target-defaults]` (cxx-std, includes,
  install, the one `public-headers` override for abseil's repo-root
  layout), `[dev-dependencies].googletest`, demo target.
- `gen_toml.py` — mines `absl_cc_library` + `absl_cc_test` calls, computes
  the closure, emits target blocks: `dev`/`test` markers 1:1 from
  TESTONLY/absl_cc_test, cfg link-flags sub-tables for the
  platform-conditional LINKOPTS (`# transcribed:` comments), per-test
  ABSL_TEST_COPTS deltas under `[targets.<t>.cfg.clang/gcc]`.
- `overlay/` — files added to the upstream checkout (demo main, stub TU —
  header-only targets still need it; interface-library is B10/wave 2).
- `consumer/` — the dependency-consumption project (no substitution step
  anymore; the manifest is checked in verbatim).
- `GAPS.md` — wave-2 edition: Dissolved (workaround → feature) + Remaining
  (including new bugs found in the wave-1 features).

## Parity evidence (all on macOS arm64, Apple clang 21)

Protocol: byte-compare demo stdout of the same `main.cpp` across FOUR
builds (wave 1 had three):

1. cpp-pkg native port (`upstream/build/demo`)
2. upstream CMake reference (Release, `ABSL_ENABLE_INSTALL=ON`,
   `ABSL_PROPAGATE_CXX_STD=ON`, `CMAKE_CXX_STANDARD=17`), demo via
   `find_package(absl)` against that install
3. cpp-pkg consumer (`consumer/build/demo`, unpatched abseil via store)
4. **round-trip** (new): plain CMake demo built via `find_package(absl)`
   against OUR `cpp-pkg install` prefix — the emitted `abslConfig.cmake`,
   not upstream's

All four outputs identical (10 lines). Verified twice: authoring workdir +
fresh `pin.sh` run (`NATIVE-REPRO-OK` / `CONSUMER-PARITY-OK` /
`CONSUMER-REPRO-OK` / `REFERENCE-PARITY-OK` / `ROUNDTRIP-PARITY-OK`).

Build health:

- Native default build: 249 ninja actions from clean (the +1 over wave 1 is
  `internal/escaping.cc`, which upstream smuggles in through HDRS and the
  generator now routes correctly); **zero compiler warnings** with the full
  upstream ABSL_LLVM_FLAGS warning set active (wave 1 dropped COPTS
  entirely). Second build: `ninja: no work to do`.
- `cpp-pkg test`: **88 passed, 0 failed** (88 invocations across 88
  targets), +197 actions on top of the default build. googletest v1.17.0 is
  fetched/built lazily on first test, store-cached after.
- `cpp-pkg install`: 504 files (93 static libs, 384 `.h` + 24 `.inc` — the
  header count byte-matches the reference CMake install —
  `lib/cmake/absl/{abslConfig.cmake,abslConfigVersion.cmake,cppkg-manifest.json}`).
- Consumer dep build ≈ 10 s; second build and cross-checkout rebuild are
  store cache hits (impossible in wave 1: the `file://` patched clone's
  per-machine commit sha made every checkout's config hash unique).
- Lockfile: committable, pins the real upstream commit.

## The ergonomics number (backlog ask)

Same 93 library targets, generator regenerated with `[target-defaults]` +
`[flags]`:

| | wave 1 | wave 2 |
|---|---|---|
| generated lines, 93 lib targets | **660** (7.1/target) | **470** (5.05/target) |

−29% — exactly the "29% of generated text is pure repetition" measured in
wave-1 GAPS (`cxx-std = 17` + `includes = { public = ["."] }` × 93, now
written once in `[target-defaults]`, plus per-target `install` and
`public-headers` it would have taken to export). The full manifest is 1548
lines, but the added 1007 lines are the dev/test section wave 1 could not
express at all (21 dev libs + 88 tests, incl. ~350 lines of per-test COPTS
deltas — the *new* headline repetition; see GAPS.md).

## Scope honesty

- Still a subset: 93/216 non-test libraries; 88/241 tests (those whose
  closure lies inside the subset — `random`/`log`/`flags`/`status` closures
  and benchmark-dependent tests stay out). No DLL mode.
- The manifest is no longer a macOS projection: `cfg.linux` (`-lrt`),
  `cfg.macos` (CoreFoundation), and the builtin `Threads::Threads` edges
  are written from upstream's own build logic. The Linux branches are
  transcribed, not yet executed — S5 validates them on a real Linux box.
- Header-only targets remain static libraries over a stub TU
  (interface-library is B10, wave 2); the 54 empty archives now also ship
  in the install prefix.
