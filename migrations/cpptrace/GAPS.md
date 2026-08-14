# GAPS — cpptrace migration, wave-2 edition

Re-migration onto the wave-1 extensions. Verdict up front: every workaround
this project carried dissolved into real syntax on the first try — the only
tool bug that *blocks* something is the Config-emitter ordering bug
(Remaining 1), and the scariest finding is that the hermeticity scan does
not actually fire (Remaining 2). Numbers below reference the wave-1 GAPS
entries.

## Dissolved (workaround → feature)

1. **zstd subdir (wave-1 gap 1, "no workaround exists")** →
   `subdir = "build/cmake"` on a url dep. Upstream's exact tarball pin
   (`CPPTRACE_ZSTD_URL`, zstd 1.5.7) configures from `build/cmake/`,
   builds, installs into the store, and libdwarf resolves it via
   `needs = ["zstd"]`. `ENABLE_DECOMPRESSION` is back to upstream's
   effective default TRUE — the wave-1 scope reduction (Linux builds
   losing compressed-`.debug_*` support) is gone.

2. **System-zlib declaration (wave-1 gap 2, first half)** →
   `[dependencies.zlib] system = true, find-package = "ZLIB"`. Probed once
   ("system dependency zlib: ZLIB 1.2.12 (76ce3af8)"), recorded as a
   machine-local sysdep store entry, folded into libdwarf's dep hash; the
   lock row is `source = "system"` — the declaration, not the machine.
   The SDK `libz.tbd` path in libdwarf's manifest is now *covered*.
   (The enforcement half — erroring when it is NOT declared — turns out
   not to work: Remaining 2.)

3. **configure_file version header (wave-1 gap 4)** →
   `[generate.version-header]`: template = `cmake/in/version-hpp.in`,
   output under `${gen}/include/cpptrace/`, vars from
   `${package.version.major/minor/patch}`. pin.sh's three-`sed` block is
   deleted; the version is stated once; the generated header ships to the
   install prefix via the public `${gen}/include` entry with zero extra
   words (the wave's flagship generated-header-export case — verified:
   `include/cpptrace/version.hpp` is in the staged prefix).

4. **Platform-conditional defines (wave-1 gap 5)** → `cfg` sub-tables.
   The macOS projection became `cfg.unix` (4 defines shared by both
   platforms) + `cfg.macos` (execinfo unwinder, mach_vm) + `cfg.linux`
   (`CPPTRACE_UNWIND_WITH_UNWIND`, `CPPTRACE_HAS_DL_FIND_OBJECT`, `-ldl`),
   each entry labeled `# transcribed: <upstream check>`. One committed
   manifest now claims both platforms; macOS verified this stage, Linux
   branches are honest transcriptions of `cmake/Autoconfig.cmake` for S5.
   Note for S5: `CPPTRACE_HAS_DL_FIND_OBJECT` assumes glibc >= 2.35;
   musl/old-glibc take upstream's `HAS_DLADDR1` fallback instead — the
   try-compile probe remains the eventual fix (reserved, not in v1).

5. **Per-target flags (wave-1 gap 6)** → target-scope `cxx-flags`.
   Upstream's visibility pair rides private on the library target only;
   the warning set splits `cfg.clang`/`cfg.gcc` exactly along upstream's
   generator expressions (GNU adds `-Wuseless-cast -Wmaybe-uninitialized`;
   tests add `-Wno-pedantic -Wno-attributes`, GNU tests
   `-Wno-infinite-recursion`). The `[profiles.relwithdebinfo]` smuggle is
   deleted; `compile_commands.json` confirms the demo and unittest TUs no
   longer see the library's flags. Both sub-issues (granularity,
   per-profile duplication) are gone.

6. **Testing story (wave-1 gap 8)** → dev/test markers + runner.
   googletest is a `[dev-dependencies]` entry at upstream's exact pin
   (v1.12.1, "last to support C++11", `find-package = "GTest"`); the
   actual ctest suite (`unittest`: 19 sources, gtest_main + gmock_main,
   private `src/` include) is `test = true` and `cpp-pkg test --config
   relwithdebinfo` builds and runs it: **155 tests / 24 suites, all
   pass**. `demo` is `dev = true` — a library consumer's default build no
   longer compiles it, and `cpp-pkg build` does no store work for
   googletest (verified: it was first provisioned only when a dev target
   was requested).

7. **Install & export (wave-1 gap 9, "disqualifying")** → `install = true`.
   `cpp-pkg install --prefix` stages 17 files: full header set *including
   the generated version.hpp*, `libcpptrace.a`,
   `cpptraceConfig.cmake`/ConfigVersion (SameMajorVersion), and
   `cppkg-manifest.json` with the libdwarf pin + options as `requires`
   and zlib as a system requirement. A find_package consumer builds and
   runs against the prefix — once the emitter bug below is fixed (the
   LINK_ONLY closure through libdwarfConfig → zstd/zlib resolves
   correctly; upstream's vendored-archive approach stays a documented
   divergence).

## Remaining

### 1. NEW BUG — emitted Config computes `_IMPORT_PREFIX` before `find_dependency`, which clobbers it (major; one-line fix)

`cpptraceConfig.cmake` is emitted in this order: compute `_IMPORT_PREFIX`
(4× `get_filename_component`), then `find_dependency(ZLIB)` +
`find_dependency(libdwarf)`, then use `${_IMPORT_PREFIX}` in the imported
target's properties. `find_dependency(libdwarf)` includes
`libdwarfConfig.cmake`, whose CMake-generated targets file ends with
`set(_IMPORT_PREFIX)` — clearing the variable **in the same scope**. Every
property then expands to `"/include"`, `"/lib/libcpptrace.a"`:

> CMake Error: Imported target "cpptrace::cpptrace" includes non-existent
> path "/include" in its INTERFACE_INCLUDE_DIRECTORIES.

Repro: install to a prefix, `find_package(cpptrace)` from any consumer.
Verified fix (by editing the staged file): move the `find_dependency`
block **above** the `_IMPORT_PREFIX` computation (what CMake's own
`configure_package_config_file` layout does) — consumer then configures,
links the full LINK_ONLY closure (libdwarf + zstd + zlib), and the binary
symbolizes correctly. Fires for any exported package with at least one
external dependency whose config uses `_IMPORT_PREFIX` — i.e. nearly
every real library; wave 1's other install cases (vtz, googletest,
benchmark) have no external deps in their Config, which is why S3 didn't
see it.

### 2. NEW BUG — hermeticity scan does not fire: undeclared SDK zlib still leaks silently (major)

The negative test of the wave's own design (§5.5 "error by default...
SDK-rooted paths are not exempt"; the design doc even names cpptrace's
leaked entry as the ingestion-time test case): delete the zlib
declaration (`[dependencies.zlib]` and the `needs` entry), fresh config
hash, rebuild. Result: libdwarf's `find_package(ZLIB)` resolves the SDK
`libz.tbd`, the absolute path
`/Applications/Xcode.app/.../MacOSX.sdk/usr/lib/libz.tbd` lands in the
store manifest **uncovered by any hash input**, and the build is green —
zero errors, zero warnings, on fresh extraction AND on the subsequent
cached-manifest read. This is byte-for-byte the wave-1 gap-2 failure mode
the scan was designed to kill. The declared-sysdep path (Dissolved 2)
works; the *enforcement* path appears unimplemented or not wired into
either probe output or ingestion. On Linux `/usr/lib` hits are the
default failure mode, so S5 will silently produce machine-dependent store
entries until this lands.

### 3. `cppkg-manifest.json` omits zstd from `requires` (minor, export fidelity)

The exported manifest's `requires` has libdwarf (source+pin+options) and
`system-requires` has zlib, but zstd — a declared url dep in the export
closure, reachable via libdwarf's `needs` and its config's
`find_dependency(zstd)` — appears nowhere. The spine's promise
("re-provision the identical dependency from the recorded pin") is
unkeepable for libdwarf from this manifest alone: re-provisioning it per
its row would configure without zstd and silently produce a
decompression-less libdwarf (or fail its find_dependency). Transitive
`needs` deps of `requires` entries should serialize too.

### 4. Dev-dep provisioning is coarser than the requested target (minor)

`cpp-pkg build demo` provisioned and built googletest although demo's
closure never reaches it (demo → cpptrace → libdwarf only). Likewise
`cpp-pkg test --list` fetched and built all deps (in a fresh config,
Release) before printing one line. Laziness holds at the build-vs-dev
boundary (Dissolved 6) but not per-target within the dev graph, and
`--list` should do no store work at all.

### 5. Carried over, still open (minor)

- **Per-config build dirs** (wave-1 misc, Appendix A item 10 "decided"):
  outputs still land flat in `./build`; my `test --list` slip in a
  Release config recompiled the relwithdebinfo tree in place, exactly the
  wave-1 complaint.
- **`cxx-extensions`** still reserved: upstream compiles at gnu++17, we
  pin strict c++17 (unchanged cosmetic delta: one extra
  `piecewise_construct` symbol).
- **Options lint** (unknown dep `options` keys, Appendix A item 10): not
  observed; a misspelled `ENABLE_DECOMPRESSION` would still silently
  rebuild with the default.
- **Windows branches not written**: MSVC/dbghelp back-end selection also
  flips the *dependency set* (no libdwarf under MSVC), which cfg cannot
  condition per-target deps + dep presence around cleanly yet; out of
  campaign scope, honest.

## What went right (worth keeping)

- The whole rewrite was mechanical: each workaround had exactly one
  designated new home, and the wave-1 GAPS entries mapped 1:1 onto
  features (this file's Dissolved section is the wave-1 file inverted).
- `exposes-targets = ["ZLIB::ZLIB"]` on the sysdep resolved the one
  ambiguity (libdwarf's probe re-exports ZLIB::ZLIB) with exactly the
  error message the spec promised — the error text contained the fix.
- Lockfile is now the complete declared universe on one page: git dep,
  url dep with blake3, `source = "system"`, dev dep — committable from
  this machine, platform-independent.
