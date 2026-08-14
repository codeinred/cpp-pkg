# Migration: ninja-build/ninja → cpp-pkg (wave-2 edition)

- **Upstream:** https://github.com/ninja-build/ninja
- **Ref:** tag `v1.13.2` (commit `3441b633c2fe2c494e958780ba0f4227b1327634`)
- **Machine:** macOS arm64, Apple clang 21, cmake 4.4, no `re2c` installed
- **Status:** GREEN for build/test/gen — full parity, zero patches, zero
  staging steps (wave 1's pin.sh pre-generation is gone). `cpp-pkg install`
  is **blocked by a new wave-1 bug** (GAPS.md R1: installing an executable
  over local static libs trips the library header-derivation error).

## What the project is

Ninja is a small build system: a single C++11 binary with no external
library dependencies, plus a GoogleTest test executable and seven perf/bench
executables. Wave 1 ported it as a macOS/posix projection with three
workaround families; the wave-1 extensions dissolve them — see GAPS.md's
Dissolved section for the full mapping.

## What the manifest now says (wave-1 features in use)

| Upstream construct | cpp-pkg spelling |
|---|---|
| `if(WIN32)/else()` source split | `[targets.libninja.cfg.unix]` / `.cfg.windows` (+ `ninja_test` cfg groups) |
| `check_cxx_symbol_exists(ppoll)` → `USE_PPOLL=1` | `[targets.libninja.cfg.linux]` public define, `# transcribed:` comment |
| global `NOMINMAX`, `_CRT_SECURE_NO_WARNINGS` | public defines on `libninja` cfg.windows / cfg.msvc |
| global `-Wno-deprecated` (non-MSVC) | `[flags.cfg.clang]` + `[flags.cfg.gcc]` (was 4× profile stanzas) |
| MSVC `/W4 /wd…` block | `[flags.cfg.msvc]` |
| `add_custom_command` browse_py.h | `[generate.browse-py-h]` (stdin/stdout command step); ninja target includes `${gen}` |
| re2c regenerate-or-fallback | `[generate.depfile-parser]`/`[generate.lexer]` with `checked-in`; `cpp-pkg gen --check` = drift guard |
| test-only FetchContent googletest | `[dev-dependencies.googletest]` (`find-package = "GTest"`); `[dependencies]` is empty |
| `add_test(NAME NinjaTest COMMAND ninja_test)` | `test = true` + `[[targets.ninja_test.run]]` with scratch cwd under `build/` |
| `find_package(Threads)` + link | `Threads::Threads` builtin edge on ninja_test |
| BUILD_TESTING perftests (never add_test'd) | seven `dev = true` targets |
| `cxx_std_11` everywhere | `[target-defaults] cxx-std = 11` (ninja_test overrides to 14) |
| `install(TARGETS ninja)` | `install = true` on `ninja` — declaration correct, execution blocked (GAPS.md R1) |

Still deliberately out: AIX/OS400 branch (no `aix` atom), IPO/LTO,
`windows/ninja.manifest` + getopt-as-C++ (Windows toolchain doesn't exist),
per-source properties for `browse.cc`.

## Reproduce

```sh
cd migrations/ninja
./pin.sh                     # clone v1.13.2, copy CppPkg.toml (nothing else)
cd upstream
export CPPKG_STORE=/tmp/cppkg-store-ninja
cpp-pkg build                # binary only; no googletest fetched/built
./build/ninja --version      # 1.13.2
./build/ninja -t list        # includes `browse` (generate edge worked)
cpp-pkg test                 # builds gtest (dev-dep) + ninja_test, runs it
                             #   from build/ninja_test-scratch: 409/409
cpp-pkg test --list          # the one declared invocation
cpp-pkg build canon_perftest # dev targets by explicit name
cpp-pkg gen --check          # re2c drift guard (needs re2c on PATH)
cpp-pkg install --prefix …   # currently fails — GAPS.md R1
```

## Parity evidence (2026-08-14, fresh store `store-s4-ninja`)

- `cpp-pkg build`: 37 edges, warning-clean, **no dev-dep work** (store
  `pkg/` empty afterward; lockfile still eagerly pins googletest at
  `6910c9d9165801d8827d628cb72eb7ea9dd538c5`, same as wave 1).
- `ninja --version` → `1.13.2`; `-t list` → identical subtool list to the
  wave-1 CMake baseline, including `browse`.
- `build/gen/build/browse_py.h` byte-identical to the upstream
  `inline.sh` recipe; touching `src/browse.py` reruns exactly the GEN edge
  (restat prunes downstream) — wave 1's silent-staleness defect is dead.
- `cpp-pkg test` → 409/409 tests from 31 suites (matches wave 1's manual
  run of the CMake and cpp-pkg binaries).
- Store determinism: after `rm -rf build`, `cpp-pkg test` rebuilt project
  TUs only; googletest served from the store under the **same key as wave
  1** (`googletest-7c9ce38684d2a5bf30da5c28e7b0cd0a`) — the `[flags.cfg.*]`
  warning flag is consumer-only and did not invalidate it. Subsequent
  `build`/`test`/`build` alternation: zero edges.
- checked-in gen plumbing verified with a stub `re2c` emitting the
  committed bytes: `gen --check` reports both lexers current; `gen` no-ops.
- `cpp-pkg install --prefix` (with and without explicit `ninja` argument,
  and under `--list`): fails with the libninja header-derivation error —
  diagnosed in GAPS.md R1 (closure loop in `src/shim.rs::plan_install`
  contradicts `validate_exported_closure`'s executable exemption).

## Files

- `CppPkg.toml` — source of truth; `pin.sh` copies it into the checkout.
- `pin.sh` — pin + copy (the codegen staging step is gone).
- `GAPS.md` — wave-2 edition: Dissolved (workaround → feature) + Remaining
  (including the new install bug).
