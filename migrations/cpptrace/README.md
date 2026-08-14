# Migration: cpptrace v1.0.4 — wave-2 edition

- Upstream: https://github.com/jeremy-rifkin/cpptrace
- Ref: tag `v1.0.4` = commit `3db8da80111171c219ab5839905771386bee06b3`
- Platform exercised: macOS arm64, Apple clang 21, `relwithdebinfo`
  (Linux branches written from upstream's Autoconfig, pending S5 validation)
- Status: **green** — library, demo parity, 155/155 unit tests via
  `cpp-pkg test`, install + find_package consumer (the last needs a
  one-line fix in the Config emitter — see GAPS.md Remaining 1).

## What changed since wave 1

Every wave-1 workaround this project carried is now real syntax:

| wave 1 | wave 2 |
|---|---|
| pin.sh `sed`-generated `gen/include/cpptrace/version.hpp` in the source tree; version stated 3× | `[generate.version-header]` template step; `${package.version.*}` interpolation; output under `${gen}/include`, public, and it **installs** via header derivation |
| defines baked for macOS only; manifest platform-specific | `cfg.unix` / `cfg.macos` / `cfg.linux` transcriptions of `cmake/Autoconfig.cmake`, each labeled `# transcribed: <check>`; one committed manifest for both platforms |
| per-target flags smuggled through `[profiles.relwithdebinfo]`, hitting every target | `cxx-flags` private on the library target (visibility + warnings), `cfg.clang`/`cfg.gcc` split exactly as upstream's generator expressions |
| `ENABLE_DECOMPRESSION=FALSE` (zstd undeclarable — CMakeLists in `build/cmake/`; zlib would leak silently) | upstream's effective default restored: zstd declared with `subdir = "build/cmake"` (same URL tarball upstream pins), zlib a declared `system = true` dep — SDK `libz.tbd` is now a *hash input*, not a leak |
| unit suite unported ("testing reduced to the demo") | `[dev-dependencies] googletest` at upstream's exact v1.12.1 pin; `unittest` target `test = true` (19 sources, gtest_main + gmock_main, `src/` internals include); `cpp-pkg test` runs it: **155 tests, 24 suites, all pass** |
| demo built as a regular target (in every consumer's default build) | `demo` is `dev = true`: default `cpp-pkg build` builds only the library, `cpp-pkg build demo` builds the demo |
| library unpublishable (no install story) | `install = true`: `cpp-pkg install --prefix` stages headers (generated `version.hpp` included), `libcpptrace.a`, `cpptraceConfig.cmake` + ConfigVersion + `cppkg-manifest.json` |

## Reproduce

```sh
cd migrations/cpptrace
./pin.sh                       # clone upstream at the pinned commit, copy manifest+lock
cd upstream
export CPPKG_STORE=/tmp/cpptrace-store
cpp-pkg build --config relwithdebinfo          # library only (45 TUs + gen step)
cpp-pkg build demo --config relwithdebinfo     # dev target, by name
./build/demo                                   # symbolized stacktrace
cpp-pkg test --config relwithdebinfo           # builds + runs unittest: 155 pass
cpp-pkg install --prefix /tmp/cpptrace-prefix --config relwithdebinfo
```

Second `cpp-pkg build` in the same store: no "building dependency" lines,
`ninja: no work to do`.

## Parity evidence (re-run this wave)

Protocol as wave 1: upstream CMake+ninja RelWithDebInfo build of
`test/demo.cpp` (with FetchContent libdwarf/zstd, i.e. decompression ON,
matching our manifest) vs `cpp-pkg build demo`; normalize hex addresses and
the dyld `start + N` offset; diff.

- **Demo outputs byte-identical after normalization** (38 lines: both
  traces, inlined-frame attribution, `file:line:col` on all project
  frames, same two benign system-dylib notes).
- Archive parity: 45 members in `libcpptrace.a` (identical to upstream's
  hand-maintained list; `src/**/*.cpp` resolves to exactly those 45).
- Scoping verified in `compile_commands.json`: library TUs get
  `-fvisibility=hidden`/warnings/macOS defines and see libdwarf headers
  privately; unittest TUs get gtest via `-isystem`, no libdwarf, no
  visibility flags; no Linux defines appear on macOS.
- Hermeticity: every absolute path in the libdwarf store manifest is
  store-rooted or the *declared* SDK zlib (`sysdeps/zlib-76ce3af8`,
  probed as `ZLIB 1.2.12`). Wave 1's silent Homebrew/SDK leak is now a
  declaration — but note the enforcement half regressed silently; see
  GAPS.md Remaining 2.
- Consumer: `find_package(cpptrace)` against the installed prefix builds
  and runs the demo — after manually reordering two blocks in the emitted
  `cpptraceConfig.cmake` (emitter bug, diagnosis + fix in GAPS.md
  Remaining 1).

## Still not ported (unchanged, deliberate)

- `integration`/`signal_demo`/`signal_tracer`/`link_test`/`c_demo`
  helper binaries: upstream registers **no ctest** for them (they are
  driven by CI scripts with expected-output harnesses); the actual ctest
  suite is exactly `unittest`, which is ported.
- C++20 module build (`HAS_CXX20_MODULES`), tools/, shared/CPack, and
  upstream's static-closure vendoring of `libdwarf.a` into its install
  prefix (deferred vendoring — documented divergence; our Config emits
  `find_dependency(libdwarf)` + `requires` pins instead).

## Files

- `CppPkg.toml` — the whole build (pin.sh copies it in)
- `CppPkg.lock` — the complete declared universe: libdwarf (git), zstd
  (url+blake3), zlib (`source = "system"`), googletest (dev)
- `pin.sh` — clone + copy only; the codegen block is gone
- `GAPS.md` — wave-2 edition: Dissolved / Remaining
