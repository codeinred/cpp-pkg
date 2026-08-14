# Migration: cppcheck 2.21.1 (CLI)

- Upstream: https://github.com/danmar/cppcheck
- Ref: tag `2.21.1` = commit `904cfdcf774c44b17db789c8a212e2f1c69fc833`
- Scope: the `cppcheck` CLI binary and its runtime data (cfg/platforms/addons).
  The Qt GUI is deliberately **out of scope** (upstream option `BUILD_GUI=OFF`
  by default), not a gap. Tests (`testrunner`) not migrated; see GAPS.md
  (testing-story).
- Status: **green** — builds via cpp-pkg only, byte-identical analysis output
  vs the upstream CMake Release build. No source patches (`patches/` empty).

## What the project is

A ~86 kLOC C++11 static analyzer: 71 sources in `lib/` (the analysis core),
11 in `cli/`, one in `frontend/`, plus three vendored externals
(`simplecpp`, `tinyxml2` compiled; `picojson` header-only). No external
package dependencies in the default build. At runtime it loads `cfg/*.cfg`
library definitions and `platforms/*.xml` relative to the executable (or a
compiled-in `FILESDIR`).

## Migration approach

See the header comment in `CppPkg.toml` for the full target mapping. Notable
decisions:

- **Vendored externals as project targets**: tinyxml2/simplecpp are ordinary
  `static-library` targets compiling `externals/...` sources in-tree;
  picojson (upstream INTERFACE lib) becomes a private include dir on its
  consumers because v0 has no `interface-library` kind.
- **`cppcheck-core` as a static library, not OBJECT**: upstream builds it as
  an OBJECT library citing static-initializer check registration; in 2.21.x
  that comment is stale (`lib/checks.cpp` registers all checks explicitly).
  Verified empirically: the static-archive build reports the identical 342
  error ids.
- **cli merged into the executable**: upstream builds a `cli` static lib from
  `GLOB *.cpp` minus `main.cpp`; cpp-pkg globs cannot exclude, so `cli/*.cpp`
  (including main) compiles directly into the exe. Link-equivalent result.
- **Matchcompiler codegen skipped** (== `-DUSE_MATCHCOMPILER=Off`) and
  **ambient Boost not used** (== `-DUSE_BOOST=Off`): both are performance-only;
  behavior verified identical. See GAPS.md.
- **Runtime data staged by script**: `stage-data.sh` mirrors upstream's
  `copy_cfg`/`copy_platforms`/`copy_addons`/`remove_unsigned_platforms`
  post-build custom targets.

## Reproduce

```sh
cd /opt/claude/cpp-pkg/migrations/cppcheck
./pin.sh                                    # clones upstream, copies CppPkg.toml
cd upstream
CPPKG_STORE=/tmp/cppkg-store-cppcheck \
  /opt/claude/cpp-pkg/target/debug/cpp-pkg build   # release config (default)
cd .. && ./stage-data.sh
./upstream/build/cppcheck --version
```

First build on an M-series Mac: ~7 s wall (90 ninja edges). Second
`cpp-pkg build`: `ninja: no work to do.` (0.15 s). The dependency store is
unused — the default cppcheck build has zero external packages — so the
"store cache hit on second build" check is vacuous here; incrementality is
carried entirely by the generated ninja file.

## Parity protocol and evidence (2026-08-14, macOS arm64, Apple clang 21)

Upstream reference: fresh `cmake -G Ninja -DCMAKE_BUILD_TYPE=Release` +
`ninja cppcheck` (matchcompiler ON via Auto, Homebrew Boost 1.90 auto-detected
by upstream — both performance-only).

1. `cppcheck --version` → `Cppcheck 2.21.1` (both).
2. `cppcheck --errorlist` → byte-identical XML, 342 `<error id=` entries
   (proves every checker registered).
3. `fixture/bugs.cpp` (deliberate bugs: uninitvar, malloc leak,
   array OOB, null deref, zerodiv, vector OOB) with
   `--enable=all --inconclusive --template={file}:{line}:{severity}:{id}:{message}`
   → 20 findings, byte-identical diff (includes cfg-dependent checks:
   `memleak`/`nullPointerOutOfMemory` require `std.cfg` to resolve `malloc`,
   proving runtime-data lookup works).
4. `--platform=avr8 --enable=portability` → identical (proves file-based
   `platforms/*.xml` lookup relative to the exe).

A third build variant (naive port with frontend+cli merged differently) also
matched byte-for-byte, so the result is layout-insensitive.

## Files

- `CppPkg.toml` — the manifest (source of truth; `pin.sh` copies it into the
  checkout).
- `pin.sh` — clone upstream at the pinned commit, stage the manifest.
- `stage-data.sh` — copy cfg/platforms/addons next to the built binary.
- `fixture/bugs.cpp` — parity fixture.
- `GAPS.md` — friction points found, keyed to the design questions.
