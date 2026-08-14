# Migration: cppcheck 2.21.1 (CLI + tests) — wave-2 edition

- Upstream: https://github.com/danmar/cppcheck
- Ref: tag `2.21.1` = commit `904cfdcf774c44b17db789c8a212e2f1c69fc833`
- Scope: the `cppcheck` CLI binary, its runtime data (cfg/platforms/addons),
  and — new this wave — the full upstream ctest suite (85 testrunner
  fixtures + 27 cfg-* library checks) plus the dev-graph helper tools.
  The Qt GUI stays deliberately out of scope (`BUILD_GUI=OFF` default).
- Status: **green** — builds via cpp-pkg only; byte-identical analysis
  output vs a fresh upstream CMake Release reference build; **exact
  warning-profile parity** (31 warnings, same buckets, both builds);
  112/112 `cpp-pkg test` invocations pass; installed prefix runs
  standalone. One new wave-1 tool bug found in `cpp-pkg install`
  (closure over-staging; workaround in the manifest — see GAPS.md).

## What changed since wave 1

Every wave-1 workaround this project carried is now real syntax; the
manifest grew from "the CLI, projected onto macOS, minus everything
upstream's build actually does" to a faithful port:

| wave-1 state | now |
|---|---|
| `stage-data.sh` (documented cp commands) | `runtime-data` on three targets (byte-equal dedupe), staged at build time and installed under `share/cppcheck/` — the script is deleted |
| `FILESDIR="/usr/local/share/Cppcheck"` hardcoded | `FILESDIR="${install-prefix}/share/cppcheck"` — rebaked per prefix, verified by running from the installed prefix |
| cli library unreproducible (no glob exclude); cli merged into the exe | `[targets.cli] sources = ["cli/*.cpp", "!cli/main.cpp"]` — the testrunner shares it, exactly like upstream |
| 4 defines + `cxx-std = 11` repeated on all 5 targets | `[target-defaults]` once |
| upstream warning policy dropped entirely (~9 stray warnings) | `-Weverything` + curated `-Wno` list under `[flags.cfg.clang]`, GNU list under `[flags.cfg.gcc]`, per-target vendored-code relaxations as target `cfg` flags — warning output now matches upstream's own build 31/31 |
| ambient Homebrew Boost silently ignored (`USE_BOOST=Off` projection) | `[dependencies.boost] system = true, find-package = "Boost"` — declared, `-isystem`, `HAVE_BOOST` on; lock row is the declaration, not the machine |
| `HAVE_EXECINFO_H=1` baked as a macOS-only literal | labeled cfg transcriptions (`[flags.cfg.unix]`, `# transcribed:` comments incl. the musl caveat); Linux branches written for S5 |
| `${CMAKE_THREAD_LIBS_INIT}` dropped | `Threads::Threads` builtin on both executables |
| tests not migrated | `testrunner` `test = true` + 85 per-fixture `[[run]]` entries; 27 cfg-* checks; helpers (`test-signalhandler`, `test-stacktrace`, `test-sehwrapper`, `dmake`) `dev = true` |

Still deliberately divergent (unchanged, see GAPS.md): matchcompiler
codegen off (tier-c per-source transform remains deferred; upstream's
`Verify` mode is the behavior-identity contract), `HAVE_RULES`/PCRE off
(reserved `pkg-config` field), no PCH.

## Reproduce

```sh
cd /opt/claude/cpp-pkg/migrations/cppcheck
./pin.sh                                    # clones upstream, copies CppPkg.toml
cd upstream
export CPPKG_STORE=/tmp/cppkg-store-cppcheck

cpp-pkg build                               # release; data staged next to the
./build/cppcheck --version                  # binary automatically — no script

cpp-pkg test --jobs 8                       # 85 fixtures + 27 cfg checks
                                            # (builds testrunner lazily)

cpp-pkg install --prefix /tmp/cppcheck-prefix
/tmp/cppcheck-prefix/bin/cppcheck --enable=all some.cpp   # FILESDIR lookup

cpp-pkg build test-stacktrace dmake         # dev-graph tools, by name
```

Cold default build: 226 ninja edges (incl. runtime-data copy edges),
~12 s wall on this M-series machine under the full `-Weverything` policy.
Second `cpp-pkg build`: `ninja: no work to do.` `cpp-pkg test` builds 81
more edges lazily. The dependency store holds only the boost sysdep
manifest entry (nothing builds — system dep). Note `install --prefix X`
rebakes `${install-prefix}` into every TU (upstream applies FILESDIR via
global `add_definitions`, so the faithful port puts it in
`[target-defaults]`) — a prefix change is a near-full recompile by
construction.

## Parity protocol and evidence (2026-08-14, macOS arm64, Apple clang 21)

Reference: fresh `cmake -G Ninja -DCMAKE_BUILD_TYPE=Release` + `ninja
cppcheck copy_cfg copy_platforms copy_addons` from the same pin
(matchcompiler ON via Auto, Boost auto-detected — both perf-only).

1. `--version` → `Cppcheck 2.21.1` (both).
2. `--errorlist` → **byte-identical** XML, 342 `<error id=` entries.
3. `fixture/bugs.cpp` with `--enable=all --inconclusive
   --template={file}:{line}:{severity}:{id}:{message}` → 21 lines,
   **byte-identical** (includes cfg-dependent `memleak` /
   `nullPointerOutOfMemory`, proving `std.cfg` resolution).
4. `--platform=avr8 --enable=portability` → **byte-identical** (file-based
   platform XML lookup).
5. New this wave — warning parity: both builds emit exactly 31 compiler
   warnings: 28 `-Wthread-safety-negative` + 1
   `-Wreserved-macro-identifier` (+2 notes). The transplanted
   `-Weverything` policy is upstream's to the warning.
6. New this wave — install: `cpp-pkg install --prefix <scratch>`, then run
   `<scratch>/bin/cppcheck` from an unrelated cwd: cfg-dependent findings
   and `--platform=avr8` both resolve via the baked FILESDIR (the
   build-tree copy is out of reach). `--destdir` untested here.
7. `cpp-pkg test --jobs 8`: **112 passed, 0 failed** (85 fixtures + 27
   cfg checks; `TestSymbolDatabase` spot-checked standalone).

## Files

- `CppPkg.toml` — the manifest (source of truth; `pin.sh` copies it into
  the checkout). Header comments map every target to its upstream
  CMakeLists origin; `# transcribed:` comments label configure-time
  answers per the wave-1 convention.
- `pin.sh` — clone upstream at the pinned commit, stage the manifest.
- `fixture/bugs.cpp` — parity fixture.
- `GAPS.md` — wave-2 edition: dissolved workarounds and what honestly
  remains (including new bugs found in the wave-1 features).
- `stage-data.sh` — **deleted** (dissolved into `runtime-data`).
