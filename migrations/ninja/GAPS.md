# Gaps found migrating ninja v1.13.2

Ordered by severity. Every item is tagged with a design question. Overall:
ninja is close to cpp-pkg's sweet spot (one binary, no deps), and it still
surfaced eight distinct gaps.

## 1. Platform-conditional sources are unrepresentable — `conditional-sources` (major)

The single biggest fidelity loss. Upstream's `libninja` is 27 common sources
plus a platform block:

- `if(WIN32)`: `subprocess-win32.cc`, `includes_normalize-win32.cc`,
  `jobserver-win32.cc`, `msvc_helper-win32.cc`, `msvc_helper_main-win32.cc`,
  `getopt.c`, `minidump-win32.cc` (+ `windows/ninja.manifest` on the exes,
  + `NOMINMAX` define, + MSVC flag set `/W4 /GR- ...`)
- `else()` (posix): `jobserver-posix.cc`, `subprocess-posix.cc`
- `if(AIX/OS400)`: additionally `getopt.c` compiled as C++, `-lperfstat`,
  `__STDC_FORMAT_MACROS`
- `ninja_test` gains `includes_normalize_test.cc`, `msvc_helper_test.cc` on
  WIN32 only

Workaround: hard-wired the posix set; the manifest is macOS/Linux-only and
says so in comments. There is no way to write ONE `CppPkg.toml` that builds
ninja on two platforms. Globs cannot help (`src/*.cc` would sweep in tests,
perftests, `-win32.cc` files, `ninja.cc`, `browse.cc`, and the `*.in.cc`
re2c inputs — which also match `*.cc`); glob *exclusion* patterns would not
help either, because the win32/posix split is not lexically clean.
What is needed is some conditional mechanism — cargo-style
`[target.'cfg(windows)'.sources]` sections or a `when = "windows"` key on
list entries — plus its interaction with the config hash.

Also note the per-source-file properties in the same block
(`set_source_files_properties(src/getopt.c PROPERTIES LANGUAGE CXX)`):
cpp-pkg's language-by-extension table is a hard rule, so "compile this .c
file as C++" is inexpressible (bites only AIX/OS400 here, but MSVC projects
do this routinely).

## 2. Codegen steps have no escape hatch — `codegen-escape-hatch` (major)

Two instances, one dodged and one worked around:

- **re2c lexers** (dodged): `depfile_parser.cc`/`lexer.cc` are generated
  from `*.in.cc` by re2c ≥2 *when available*; upstream commits the outputs
  and falls back to them. This machine has no re2c, so the CMake baseline
  and cpp-pkg compiled identical committed sources. A checkout on a machine
  *with* re2c would silently diverge from upstream CMake behavior (which
  regenerates). The upstream pattern — "regenerate if tool present, else use
  committed output" — is itself worth supporting cheaply.
- **browse mode** (worked around): `add_custom_command` pipes
  `src/browse.py` through `src/inline.sh` to produce `build/browse_py.h`,
  consumed by `browse.cc` via a per-source include dir. `pin.sh`
  pre-generates it into `gen/build/` with the exact upstream command;
  verified byte-identical to CMake's output. An escape hatch would have
  needed: (a) run an arbitrary command with declared inputs
  (`src/browse.py`, `src/inline.sh`) and one declared output, (b) an
  output directory the manifest can reference as an include dir, (c) the
  command participating in incrementality (rerun when inputs change —
  pin.sh-time generation goes stale silently if browse.py is edited).
  A `[generate.<name>] command/inputs/outputs` table emitted as ordinary
  ninja build edges would have covered both instances.

## 3. No test/dev dependency separation — `testing-story` (major)

Upstream fetches googletest ONLY under `BUILD_TESTING=ON` and only when no
system GTest exists. In cpp-pkg, googletest is an unconditional
`[dependencies]` entry: `cpp-pkg build ninja` (just the binary) still
fetches and builds googletest first. For a project whose selling point is
"zero dependencies", the manifest now declares one. Wanted: a
`[dev-dependencies]`-style section pulled in only when a test target is in
the requested target set, plus a target attribute marking `ninja_test` as a
test (today nothing distinguishes it from a shipping executable), plus a
runner (`cpp-pkg test`) — upstream has `add_test(NAME NinjaTest COMMAND
ninja_test)` and CTest; here the runner is "execute build/ninja_test from a
writable scratch cwd" documented in README prose. Also no way to express
upstream's "prefer system GTest, fall back to fetch" provisioning ladder
(overlaps `dep-provisioning`).

Positive finding: `find-package = "GTest"` (dep key `googletest`, config
name `GTest`) worked exactly as designed, and `GTest::gtest` resolved via
ladder step 1 with no `exposes-*` needed.

## 4. Configure checks have no equivalent — `conditional-sources` / `per-target-flags` (major)

Upstream runs `check_cxx_symbol_exists(ppoll ...)` → `USE_PPOLL=1`,
`check_symbol_exists(fork/pipe)` + an `execute_process` probe of
`inline.sh` → browse support, `check_cxx_compiler_flag(-Wno-deprecated)`,
`check_ipo_supported()`. On macOS the right answers are static (no ppoll,
browse yes, flag yes), so I baked them in — but the manifest encodes the
*answers*, not the *questions*: it would produce a subtly wrong binary on
Linux (no `USE_PPOLL` → ninja falls back to pselect, a real behavior
change upstream guards against). v0 can reasonably say "no configure
checks", but the gap deserves a name: platform predicates richer than
os-name (has-symbol X) are what these reduce to.

## 5. Per-target/per-source compile flags — `per-target-flags` (minor, with sharp edges)

- `-Wno-deprecated` is global upstream. The only cpp-pkg slot is per-profile
  consumer `cxx-flags`, so covering all `--config` values takes FOUR
  identical `[profiles.*]` stanzas (see CppPkg.toml). Without it the build
  works but emits sprintf-deprecation warnings the CMake build doesn't —
  warning-hygiene parity required flags. A profile-independent
  `[package].cxx-flags` (or per-target `cxx-flags`) would collapse this.
- Per-source properties: upstream sets `NINJA_PYTHON="python"`,
  `NINJA_HAVE_BROWSE`, and a binary-dir include ONLY on `browse.cc`
  (`set_source_files_properties`). Workaround: hoisted to target-private on
  the 2-file `ninja` target — harmless here, unsound in general (defines
  leak to `ninja.cc`).
- Quoted define values (`NINJA_PYTHON="python"`) survived shell_word
  escaping into build.ninja correctly — good.
- IPO/LTO: upstream turns it on for Release when supported. No cpp-pkg
  switch; could be hand-rolled as `-flto` profile flags but was left out
  (348 KB vs 397 KB binary). A profile-level `lto = true` would match how
  cargo spells it.

## 6. OBJECT libraries — `object-libraries` (minor here, real elsewhere)

`libninja`/`libninja-re2c` are CMake OBJECT libraries; cpp-pkg offers only
`static-library`. Semantic difference: archives pull members on
undefined-symbol demand, object libs link every object unconditionally.
Ninja has no self-registering translation units, so archives are
observationally equivalent (409/409 tests confirm) — but a project relying
on static initializers for registration (gtest's own `TEST()` macros
*inside a library*, plugin registries) would silently lose tests/plugins.
Worth either an `object-library` kind or a per-edge `whole-archive`
attribute before someone hits this as a wrong-behavior bug rather than a
build error.

## 7. No install/export — `install-export` (minor for this migration)

Upstream ends with `install(TARGETS ninja)`; a `cmake --install` puts the
binary in `<prefix>/bin`. cpp-pkg has no `install` verb and no way to
declare that `ninja` is the package's installable product (vs `ninja_test`
and seven perftests, which are not). Scope was reduced: the migrated build
stops at `build/ninja`. For a tool like ninja, "install the binary" is the
entire packaging story.

## 8. Ergonomics notes — `schema-ergonomics` (minor)

- Upstream's 4-line `foreach(perftest ...)` loop became seven 5-line
  stanzas. Some batch form (shared-field target templates, or accepting
  that a future scripting layer handles it) would help; a cargo user would
  reach for `[[bin]]`-style arrays.
- The four duplicated `[profiles.*]` stanzas for one global warning flag
  (item 5) read as boilerplate.
- Explicit 29-entry source lists are verbose but honest — and with comments
  they double as documentation; this is acceptable, not a defect.
- `Threads::Threads` appeared in googletest's extracted manifest
  (GTestConfig's `find_dependency(Threads)` runs inside the probe, and the
  imported target is attributed to googletest — `origin_find_name: GTest`).
  Upstream links `ninja_test` against `Threads::Threads` explicitly; on
  macOS pthreads are in libSystem so omitting the edge works, but the
  accidental attribution means a manifest author COULD write
  `dependencies = ["Threads::Threads"]` and get a target owned by a package
  that merely mentioned it. Attribution of module-package
  (`FindThreads`-style) targets deserves a deliberate rule
  (`dep-provisioning`).

## What worked without friction

Explicit-list static libraries and executables, public/private include and
define splits, target-name charset (underscores in `ninja_test` fine),
`find-package` probe-name override, tier-2 extraction of an installed GTest
(no ALIAS problem — unlike curl's `CURL::libcurl`), `cxx-std` per target
(11 for the binary, 14 where gtest requires it), lockfile pinning, store
cache hits across full build-dir wipes, and consumer-only profile flags not
invalidating dependency store entries.
