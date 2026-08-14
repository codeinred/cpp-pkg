# Gaps — googletest v1.18.0, wave-2 edition

Re-migration against the wave-1 extensions (2026-08-14). Status: **green** —
build, `cpp-pkg test`, install/export fixpoint, and store cache hit all
verified. Wave-1 gap numbers referenced as W1-#.

## Dissolved (workaround → feature)

- **W1-1 (no `cpp-pkg test`)** → `test = true` on the 10 samples +
  `cpp-pkg test`. The shell for-loop and hand-aggregated exit codes are
  gone: one command builds and runs the suite, `10 passed, 0 failed`
  summary, `--list`, name filters, `-- --gtest_*` passthrough (verified),
  `--jobs`. A filter matching nothing is the promised hard error listing
  all test targets.
- **W1-2 (no dev-dependencies)** → consumer moved googletest to
  `[dev-dependencies]`. `cpp-pkg build` of the consumer does zero store
  work ("no targets to build"); the dep is locked eagerly (CppPkg.lock row
  unchanged in grammar) and fetched/built only by `cpp-pkg test`. The
  public dependency set of a library consumer is clean again.
- **W1-3 (option-gated samples build for everyone)** → subsumed by the test
  markers exactly as wave 1 predicted: `cpp-pkg build` is now 4 libraries /
  8 ninja steps; the samples build only under `cpp-pkg test`. Upstream's
  `-Dgtest_build_samples=ON` gate needed no features/options machinery.
- **W1-5 (a cpp-pkg-built package cannot be published)** →
  `[export] cmake-name = "GTest", namespace = "GTest"` +
  `[target-defaults] install = true` + `cpp-pkg install --prefix`. Emits
  `lib/*.a`, the exact upstream header set (38 files, byte-identical set to
  `googletest/include` + `googlemock/include`), `GTestConfig.cmake`,
  `GTestConfigVersion.cmake` (SameMajorVersion, `find_package(GTest 1.18)`
  accepted), and `cppkg-manifest.json`. Fixpoint verified: a raw CMake
  consumer builds and passes against our emission — adopting CppPkg.toml no
  longer orphans the `find_package(GTest)` ecosystem. The
  `public-headers` total override keeps `googletest/` (the source dir,
  public for `#include "src/gtest.cc"` and upstream interface parity) out
  of the installed set — exactly the split upstream expresses with
  `$<BUILD_INTERFACE:...>` vs `$<INSTALL_INTERFACE:include>`.
- **W1-7 (no per-target flags; upstream warning sets dropped)** →
  `[flags.cfg.clang]`/`[flags.cfg.gcc]` carry `cxx_base_flags` (+
  `-fexceptions`, i.e. upstream's `cxx_default`, all targets);
  `cxx_strict_flags` are private per-target `cxx-flags` under
  `cfg.clang`/`cfg.gcc` on the 4 libraries only — upstream's scoping, line
  for line, including the gcc branch (`-Wextra -Wno-unused-parameter
  -Wno-missing-field-initializers`, `-Wno-error=dangling-else`) written now
  for Linux validation. Bonus: cpp-pkg's `clang` matches AppleClang, so
  the strict set actually *applies* here (0 warnings emitted, verified via
  `-fsyntax-only`), where upstream's `STREQUAL "Clang"` footgun compiled
  warning-free by accident. Same artifacts, honest diagnostics.
- **W1-8 (no SYSTEM interface includes)** → `system-includes = true` on the
  4 libraries: sample compile lines show `-isystem <...>/googletest/include
  -isystem <...>/googletest`, matching upstream's
  `target_include_directories(... SYSTEM INTERFACE ...)`.
- **W1-9 (`find-package` undocumented)** → documented in CPPKG_TOML.md
  (wave-1 tool fix 4); the consumer's comment now cites the doc instead of
  src/schema.rs.
- **W1-10 (`cxx-std = 17` × 14)** → `[target-defaults] cxx-std = 17`, once.
  The silent-default hazard (forgetting one target) is gone; the exported
  Config carries `cxx_std 17` per component like upstream's PUBLIC compile
  feature.
- **W1-11 (absorbed `Threads::Threads`)** → builtin. The libraries now
  declare the upstream PUBLIC `Threads::Threads` edge (previously dropped);
  it expands to nothing on macos, `-pthread` on linux — upstream's
  effective behavior — and the emitted Config reproduces upstream's
  `find_dependency(Threads)` + `INTERFACE_LINK_LIBRARIES Threads::Threads`
  shape exactly. No `exposes-*` tie-breaking needed anywhere.

## Remaining

### 1. Platform-conditional default define inexpressible: `[target-defaults.cfg.*]` reserved (minor)

Upstream computes `GTEST_HAS_PTHREAD=1|0` once (pthreads found?) and folds
it into `cxx_base_flags` for *every* target. The natural spelling —
`[target-defaults.cfg.unix] defines = ...` — is reserved. Choices: 14
per-target `cfg.unix` blocks (recreating the repetition B9 exists to kill)
or one unconditional default. This port chose the unconditional default
(correct on macos/linux; wrong for a future Windows toolchain, where
upstream defines `=0`). First concrete case for un-reserving
`[target-defaults.cfg.*]`, or for a package-scope conditional defines home.

### 2. Strict-flag set repeated 8 times (minor, ergonomics)

`cxx_strict_flags` applies to the 4 libraries but not the samples — an
environment statement over a *subset* of targets. `[flags]` hits every
target; flag keys in `[target-defaults]` are reserved (pointing at
`[flags]`, which is the wrong scope here); so the 11-flag clang list and
3-flag gcc list are pasted into 4 targets × 2 compilers = 8 cfg blocks.
Upstream writes each set once (`cxx_strict` variable). Wants either named
flag sets or `[target-defaults]` flag keys with dev/test eligibility rules.

### 3. compile_commands.json reflects only the last requested build set (minor, NEW wave-1 behavior)

After `cpp-pkg test sample7_unittest`, `build/compile_commands.json`
shrinks to 3 entries (gtest-all, gtest_main, sample7) — the default build's
entries are gone until the next `cpp-pkg build`/full `test`. Laziness of
the test-set plan leaks into the tooling surface: clangd loses flags for
every file outside the last-requested set. Accumulate/merge per target
rather than regenerate per invocation.

### 4. MSVC branch not transcribed (deliberate; blocked on options)

Upstream's MSVC path is more than flags: CRT selection
(`MultiThreaded$<$<CONFIG:Debug>:Debug>`) is gated on the
`gtest_force_shared_crt` *option*, which has no cpp-pkg expression, and the
base set (`-GS -W4 -wd4251 ... -D_UNICODE -DUNICODE ...`) mixes flags with
defines that the schema wants in `defines`. Left as a header comment;
Windows toolchains are out of v1 scope anyway. Becomes real work if/when a
windows toolchain lands.

### 5. Mode (a)/(b) asymmetry persists, now narrower (minor; W1-4)

In-tree test code references `gtest_main`; consumer code references
`GTest::gtest_main`. The export story removed the *publishing* half of
W1-4, but a cpp-pkg GIT dep still builds through the dependency's CMake
build — googletest-as-dep works only because upstream's CMakeLists.txt
still exists. A CppPkg.toml-only package is installable (verified) but not
yet consumable as a cpp-pkg GIT dependency; `path`/workspace deps remain
unimplemented (recorded intent in CPPKG_TOML.md).

### 6. Per-artifact version metadata / pkg-config (minor; W1-6, recorded loss)

Upstream sets `VERSION` on the archives and installs `gtest.pc` etc.
`.pc` emission is explicitly out of wave-1 scope ("out of scope,
honestly"). Unchanged.

## Notes for S5 (Linux validation)

- `[flags.cfg.gcc]` + per-lib `cfg.gcc` strict flags are transcriptions of
  upstream's `CMAKE_COMPILER_IS_GNUCXX` branch (gcc ≥ 7 form, incl.
  `-Wno-error=dangling-else`); expect them to fire under gcc 16 — any
  unknown-warning failure there is data, not expected.
- `Threads::Threads` must expand to `-pthread` on compile and link of all
  four libraries and every sample.
- `GTEST_HAS_PTHREAD=1` is unconditional by choice (Remaining #1) —
  correct on Linux.
