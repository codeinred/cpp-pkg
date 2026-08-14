# Gaps found migrating cppcheck 2.21.1

Result was green, so everything below is friction/workaround data, not a
blocker. Ordered roughly by how much they'd matter to the next project.

## 1. No codegen escape hatch (codegen-escape-hatch, major)

Upstream's non-Debug builds run `tools/matchcompiler.py` over **every** file
in `lib/` via `add_custom_command`, producing `build/mc_<file>.cpp`, and
compile those instead of the originals. It is a source-to-source optimizer
(rewrites `Token::Match(tok, "...")` string patterns into generated C++
matchers) — behavior-identical by design (upstream ships a `Verify` mode
asserting exactly that), but it is the performance-relevant configuration
that upstream releases ship.

cpp-pkg has nothing to express "run tool T over source S producing S', then
compile S'". Workaround: build the equivalent of `-DUSE_MATCHCOMPILER=Off`
(compile the plain sources). Pre-generating 71 `mc_*.cpp` files into
`patches/` was rejected as unreasonable bulk — this is the case where the
per-source scale makes the "pre-generate by hand" v0 workaround collapse.

What the escape hatch would have needed here:
- per-source transform: command template with `{input}`/`{output}` holes,
  applied over a glob (not a single fixed file);
- outputs live in the build dir and take the place of the input in the
  target's source list;
- dependency edges: regenerate when the input file OR the generator script
  (`tools/matchcompiler.py`) changes — upstream declares both `DEPENDS`.

## 2. No object-library kind / whole-archive link (object-libraries, minor here — but only by luck)

Upstream builds `cppcheck-core` as `add_library(... OBJECT ...)` with the
comment "auto-registration doesn't work with static libraries". In 2.21.x
this rationale is **stale**: `lib/checks.cpp` explicitly instantiates every
check (`CheckInstances::get()`), so a plain static archive works — verified
(`--errorlist` byte-identical, 342 ids, from a static-lib build).

The gap is real for the pattern the comment describes: a project whose
registrations live in static initializers of otherwise-unreferenced archive
members would silently lose them, and cpp-pkg has no `object-library` target
kind, no whole-archive/force-load per-edge attribute (the reserved table form
`{ target = ..., ... }` for dependency entries is the natural home), and no
way for a migrator to even detect the silent misbehavior except output diffs.
"Silently wrong, not an error" is the worst failure shape; worth supporting
before a project that genuinely needs it (LLVM passes, test frameworks,
plugin registries).

## 3. Runtime data files next to the binary (install-export, major)

cppcheck loads `cfg/*.cfg`, `platforms/*.xml`, `addons/*` at runtime,
searching the compiled-in `FILESDIR` and then the executable's own directory.
Upstream handles this twice:
- build tree: `add_custom_target(copy_cfg ALL ...)` copies the dirs next to
  the binary (plus `remove_unsigned_platforms` pruning two files);
- install: `install(FILES ... DESTINATION ${FILESDIR_DEF}/cfg)` etc., with
  `FILESDIR` a configure-time option baked into the binary as a define.

cpp-pkg has neither a post-build copy step nor any install story, so the
build "succeeds" but the binary is broken until data is staged (observed:
zero findings — cppcheck exits early when `std.cfg` is missing). Workaround:
`stage-data.sh` (documented cp commands). What would be needed natively:
a declarative `[targets.X.runtime-data]` (copy globs next to the output) —
which would then be the same metadata an eventual `cpp-pkg install` needs for
`share/`-style data. Note the FILESDIR pattern also implies install-prefix
knowledge at *compile* time (a define containing the prefix path), which any
install design must be able to feed back into compilation.

## 4. No per-target compile flags (per-target-flags, major)

Three upstream needs, none expressible:
- global warning policy: `-Weverything` + a large curated `-Wno-*` list for
  Clang (compileroptions.cmake), applied to all targets;
- per-target relaxations for vendored code: tinyxml2 gets seven extra
  `-Wno-*` (guarded by `check_cxx_compiler_flag`); simplecpp gets one;
- per-source: `processexecutor.cpp` gets `-Wno-reserved-identifier` under
  exactly Clang 13.

v0 only has profile-level `cxx-flags` (global, config-keyed). The build still
succeeds — the cost is ~9 stray warnings (e.g. `-Wmultichar` noise from
`calculate.h`, upstream silences it with `-Wno-multichar`) and no way to run
a warnings-as-errors policy that exempts vendored code. Wanted:
`[targets.X] cxx-flags = [...]` (private; maybe `public` for the rare
interface-flag case) — the same `{public,private}` shape as defines. The
`_safe` variants also show upstream needs *conditional* flags ("add if the
compiler accepts it"), which a static manifest cannot express; a `cxx-flags`
that tolerates unknown-warning flags (clang `-Wno-unknown-warning-option`
semantics) would cover 90% of it.

- Related, same bucket: upstream uses `target_precompile_headers` on the two
  big targets (build-speed only; skipped) — no PCH support in cpp-pkg.

## 5. Glob exclusion / composition (conditional-sources, major)

Upstream's `cli` library is `file(GLOB srcs "*.cpp")` **minus** `main.cpp`
(`list(REMOVE_ITEM)`), because `main.cpp` belongs to the executable. cpp-pkg
globs have no exclude form and source lists cannot be composed, so the
`cli` static library cannot be reproduced; workaround was merging `cli/*.cpp`
into the executable (link-equivalent, but the layout diverges and any second
consumer of the cli library — the testrunner is one! — would force explicit
10-file source lists). Wanted: `sources = ["cli/*.cpp", "!cli/main.cpp"]` or
an `exclude = [...]` key.

Also observed (works fine, note for the design question): cppcheck's
platform-conditional code is all *in-source* `#ifdef` (e.g. `sehwrapper.cpp`
compiles to nothing outside Windows), upstream globs everything on every
platform, and only `version.rc` is appended conditionally (`if(WIN32)`).
So this project needed no conditional source *lists* on macOS — vendored-glob
projects often push conditionality into the preprocessor.

## 6. Configure-time feature detection (dep-provisioning, major)

Three upstream configure-time probes had to be resolved by hand and baked in
as literals, making the manifest **machine/OS-specific**:

- `check_include_file_cxx(execinfo.h HAVE_EXECINFO_H)` → hardcoded
  `HAVE_EXECINFO_H=1` (true on macOS; wrong on e.g. musl).
- `find_package(Boost)` under `USE_BOOST=Auto`: upstream silently picks up
  *ambient Homebrew Boost* and compiles with `-DHAVE_BOOST`
  (boost::container::small_vector swap, performance-only). cpp-pkg can
  neither probe for a system package nor express "use it if present"; port
  pins the Boost-free configuration. Declaring Boost in `[dependencies]`
  would be the cpp-pkg-native answer but means building Boost from source
  for a header-only nicety.
- `HAVE_RULES=ON` (non-default, not migrated) wants system PCRE via bare
  `find_path`/`find_library` — no CMake config file, so even tier-2
  extraction has nothing to probe; cpp-pkg has no "system library" dependency
  form at all. Same story as `find_package(Threads)`/
  `${CMAKE_THREAD_LIBS_INIT}` (harmlessly empty on macOS).

The hermetic counter-position ("declare everything") is coherent, but the gap
is that today the *manifest author* silently becomes the configure step, and
nothing records that `HAVE_EXECINFO_H=1` is a macOS-only answer.

## 7. Project-wide defaults (schema-ergonomics, minor)

Upstream sets global `add_definitions(...)` (4 defines) and a global C++
standard once. The port repeats an identical 4-define `defines.private` line
and `cxx-std = 11` on **all five targets** — the single ugliest thing in the
resulting manifest, and it invites drift bugs (forget one target, get a
subtly different binary — upstream applies them to externals too).
Wanted: a `[defaults]`/`[package]`-level `defines`/`cxx-std` that targets
inherit. Cargo users expect workspace-level inheritance
(`[workspace.package]`); C++ natives expect directory-scoped
`add_definitions`.

## 8. Vendored-code pattern (schema-ergonomics, minor — works, worth blessing)

`externals/{simplecpp,tinyxml2,picojson}` are not installable CMake packages
(picojson is a bare header; upstream wraps each in a tiny `add_library`).
Compiling them as project targets from in-tree sources was frictionless and
is clearly the right v0 pattern — with two caveats: (a) header-only vendored
code has no `interface-library` kind, so picojson degrades into consumers'
include dirs (include-path hygiene lost: any target with the include can
use it without declaring anything); (b) per-target warning relaxation for
vendored code needs gap #4, and upstream's `EXTERNALS_AS_SYSTEM` option
(SYSTEM includes) has no equivalent — cpp-pkg `includes` has no
`is-system` attribute.

## 9. Testing story (testing-story, major — observed, not attempted)

Not migrated (scope was the CLI), but the shape of what upstream `test/`
needs is good data: `testrunner` is an executable built from ~80 test
sources **plus the cli library minus main** (gap #5 again), registered via
`add_test` per suite with `--tinyxml2` style args, needs the same runtime
data staging (cfg/platforms next to the test binary), and CTest wiring
(`REGISTER_TESTS`). cpp-pkg has: no test target kind, no runner, no
`cpp-pkg test`. A cargo user's expectation (`[targets.X] type = "test"` +
`cpp-pkg test` running them with cwd/data conventions) maps cleanly onto
what this project does by hand.

## 10. Build scheduling hint lost (schema-ergonomics, trivia)

Upstream reorders the source list to put the three slowest files
(`valueflow.cpp`, `tokenize.cpp`, `symboldatabase.cpp`) first so ninja
schedules them early. cpp-pkg globs are sorted lexicographically; no way to
express priority. Pure build-latency trivia; noting because scale projects
care.

## Non-gaps worth recording

- `FILESDIR="/usr/local/share/Cppcheck"` — a define whose value is a quoted
  C string literal — round-tripped through TOML → ninja → shell correctly
  on the first try (`'FILESDIR="..."'`).
- Private static-lib deps propagating as link-only matched CMake
  `$<LINK_ONLY:...>` semantics exactly; the 4-level static-lib chain
  (tinyxml2/simplecpp ← core ← frontend ← exe) linked correctly with no
  effort.
- Profile `cxx-flags = ["-O2"]` landed after the built-in `-O3 -DNDEBUG`,
  so last-flag-wins reproduced upstream's Release `-O2` override — but note
  this relies on flag *ordering*, which is implicit contract, not documented
  schema.
- 90-edge build, cold, ~7 s wall on M-series; `--query`/compile_commands.json
  worked (used for parity flag verification).
