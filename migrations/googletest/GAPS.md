# Gaps found migrating googletest v1.18.0

Both consumption modes worked end-to-end; every gap below had a documented
workaround or was cosmetic. Ordered by severity within each lens.

## testing-story (the sharpest data from this project)

### 1. No `cpp-pkg test` — tests are just executables you remember to run (major)

Mode (a), in-tree: the 10 sample tests are ordinary `executable` targets.
Building them is fine; *running* them is `for i in $(seq 1 10);
build/sample${i}_unittest; done` by hand, with the user aggregating exit
codes. There is no runner, no pass/fail summary across binaries, no
`--gtest_filter` passthrough, no parallelism, no "which targets are tests"
marking. Upstream gets all of this from CTest (`enable_testing()` +
`ctest`). Minimal v1 shape suggested by this migration: a `[targets.X]
test = true` marker (or `type = "test"`) + `cpp-pkg test [filter]` that
builds marked targets and runs them with per-binary and aggregate reporting.
GoogleTest binaries already exit non-zero on failure and self-report; the
runner only needs spawn/collect.

### 2. No test-only/dev dependencies (major)

Mode (b): the consumer declares googletest in plain `[dependencies]`. For a
library project this poisons the public dependency set — there is no
`[dev-dependencies]`, so a downstream consumer of the library would resolve
(and possibly build) googletest. Cargo users will reach for
`[dev-dependencies]` on day one.

### 3. No option-gated targets (major; also conditional-sources)

Upstream builds samples only under `-Dgtest_build_samples=ON` and its own
test suite under `-Dgtest_build_tests=ON`. `CppPkg.toml` has no
options/features, so all 10 samples build unconditionally for every user of
this manifest. Workaround: accept the extra ~10 targets (cheap here); a real
project with heavy test suites cannot. A tests marker (gap 1) that builds
test targets only under `cpp-pkg test` would subsume most of this;
Cargo-style `[features]` is the general fix.

### 4. Mode (a) vs mode (b) asymmetry

In-tree consumption references bare target names (`gtest_main`); GIT-dep
consumption references `GTest::gtest_main` and needed `find-package =
"GTest"` + a full CMake configure+build+install+probe (~minutes on first
build). Same code, two different names and cost profiles. Fine as v0
architecture, but a future `path`-dep or "workspace member" form should make
an in-tree package consumable under its exported names so test code doesn't
change when a library is split out.

## install-export

### 5. A cpp-pkg-built package cannot be published to anyone (major)

The migrated build produces `libgtest.a` etc. in `build/`, but there is no
`cpp-pkg install`/export: no header staging, no manifest/Config emission for
*own* targets (the shim emitter only serves CMake-*extracted* manifests).
Consequence: if googletest itself adopted CppPkg.toml, its entire ecosystem
(CMake `find_package(GTest)`, cpp-pkg GIT deps, pkg-config users) would lose
their consumption path — mode (b) works today only because upstream's CMake
build still exists to install `GTestConfig.cmake`. The IR is already
manifest-shaped; emitting `cppkg-manifest.json` + a Config shim from local
`[targets]` looks like the natural closing of the loop (the round-trip
fixpoint idea in CLAUDE.local.md).

### 6. Versioned-archive property not expressible (minor)

Upstream sets `VERSION ${GOOGLETEST_VERSION}` on the libraries and installs
pkg-config files (`gtest.pc`). No observable difference for static archives
on macOS, but an install story will need somewhere to put version metadata
per artifact.

## per-target-flags

### 7. No per-target compile flags (major in general, moot on this machine)

Upstream compiles the four libraries with a strict warning set (`-Wall
-Wshadow -Wconversion -Wundef -W -Wpointer-arith -Wcast-qual ...`) and
samples with a milder set — per-target `COMPILE_FLAGS`. cpp-pkg offers only
profile-level flags (all consumer targets uniformly). Workaround: drop the
warning flags entirely; no artifact/behavior change. Amusing evidence for
"warnings are per-target, not per-project": upstream's own branches test
`CMAKE_CXX_COMPILER_ID STREQUAL "Clang"`, which AppleClang fails, so the
reference CMake build *also* compiled warning-free here — parity by
accident. A `cxx-flags = [...]` key under `[targets.X]` (private-only, like
Cargo's per-target rustflags absence suggests keeping it simple) would have
covered this exactly. On GCC/Linux the migration would silently lose
upstream's `-Wextra` set — same artifacts, different diagnostics.

### 8. No SYSTEM interface includes (minor)

Upstream exports its include dirs with `SYSTEM INTERFACE` so consumers'
aggressive warnings don't fire inside gtest headers. cpp-pkg emits plain
`-I` for sibling-target public includes. Invisible until per-target warnings
(gap 7) exist; when they do, public includes of dependencies should probably
become `-isystem`.

## schema-ergonomics

### 9. `find-package` exists but is undocumented (major, docs-only)

First consumer build failed: probe ran `find_package(googletest)` while
upstream installs `GTestConfig.cmake`. The fix — `find-package = "GTest"` on
the dependency — exists in the implementation (`src/schema.rs`: "find_package
name used by the probe; defaults to the dep key") but appears nowhere in
CPPKG_TOML.md. The error message does hint at it (cli.rs suggests
`find-package = ...`), but only in the provider-mode path; the probe-failure
error I hit was raw CMake "Could not find a package configuration file".
Two fixes: document the field, and translate the probe's config-file-not-
found error into the same actionable hint.

### 10. `cxx-std` does not propagate; repeated 14 times (minor)

Upstream declares `cxx_std_17` as a PUBLIC compile feature once; every
consumer inherits it. In CppPkg.toml each of the 14 targets carries
`cxx-std = 17`, and forgetting one produces a working-by-luck build at the
toolchain default std. Wants either a `[package] cxx-std = 17` default or
public propagation of a library's cxx-std to dependents (max over the
closure, the CMake compile-features model).

## dep-provisioning

### 11. find-module targets absorbed into the package manifest (minor, worked)

`GTestConfig.cmake` runs `find_dependency(Threads)`; the probe's
IMPORTED_TARGETS diff attributes `Threads::Threads` to the googletest
package, and the store manifest exports it as a `GTest` component with empty
interface (macOS: threads are in libSystem). Harmless here — but on Linux
`Threads::Threads` carries `-pthread`, and if two packages both absorb it,
namespace-attribution (ladder step 3) may need the user to break the tie for
a target no one really "owns". System find-modules (Threads, and eventually
OpenSSL/ZLIB find-module fallbacks) may deserve a builtin-recognized list.

## codegen-escape-hatch / object-libraries

Not exercised: googletest has no generated sources and no object libraries.
No data from this migration.
