# Migration Wave 1 — Consolidated Backlog

Synthesis of 8 real-world migrations (2026-08-13/14, macOS arm64, Apple
clang 21). Sources: per-project `GAPS.md` + `README.md` under
`migrations/<key>/`. Severity and evidence are cited by project name;
every claim traces to a GAPS.md section.

---

## 1. SCOREBOARD

| Project | Status | Story |
|---|---|---|
| vtz | green | 774/774 tests parity incl. death tests; byte-identical tzdb codegen verification; needed 2 locally-patched deps, an aux fetch script, and double-compiles its OBJECT lib. |
| ninja | green | 409/409 tests, zero patches; cpp-pkg-built ninja then built cpp-pkg itself; manifest is a posix-only projection and gtest became an unconditional dep of a zero-dep project. |
| cppcheck | green | 86 kLOC, byte-identical `--errorlist` (342 ids) on the first build attempt; matchcompiler codegen and runtime-data staging (silent-broken until staged) are the sharp edges. |
| json-tui | green | All 4 FetchContent deps became declared deps, 6/6 byte-identical parity checks; the only wave-1 port where a gap **broke the build** (-Werror + dep `-I` vs `-isystem`). |
| googletest | green | Both modes (native port + git dep) work; exposed the undocumented `find-package` field and the "tests are executables you remember to run" hole. |
| benchmark | green | Bit-identical object files vs CMake; sharpest single finding: store strips `.git`, so upstream's `git describe` silently mis-versions rev pins — the hatch needs `${pin.*}` facts, not command execution. |
| abseil | green | 93-target native port via a 150-line generator + 217-component probe extraction; found the one wave-1 **blocker**: upstream's `absl::strings` self-edge is rejected as a cycle and there is no patch mechanism. |
| cpptrace | green | Byte-identical stack traces through a declared libdwarf dep; zstd undeclarable (CMakeLists in `build/cmake/`) and the Homebrew-zstd hermeticity leak was caught red-handed. |

8/8 green. But: every manifest is a macOS projection, no test suite was
migrated as a test suite, and no port can be consumed by anyone else.

---

## 2. RANKED BACKLOG

Rank = frequency across projects × severity. Tool-bug fixes (no schema
surface) are ranked alongside schema gaps because they cost migrations the
same hours.

### B1. Per-target flags + flag layering — 8/8 projects, one actual build break

- **Hit by:** all eight. json-tui (GAPS §4/§4b: `tests` inherited profile
  `-Werror`, gtest header warning became a hard error — the only wave-1
  build break), abseil (COPTS dropped wholesale; CoreFoundation link flag
  hoisted to profile scope, silently lost under `--config debug`),
  benchmark, cppcheck (vendored-code `-Wno-*` relaxations), vtz (per-source
  `-U/-D` was the sole scope reduction), ninja, googletest, cpptrace.
- **Sub-gaps:** (a) no `cxx-flags`/`link-flags` on targets; (b) no
  profile-independent flag layer — identical stanzas pasted into 2–4
  `[profiles.*]` blocks in ninja, benchmark, json-tui, vtz, cpptrace;
  (c) dependency interface includes emitted as `-I` not `-isystem`
  (json-tui §4b, googletest §8, cppcheck `EXTERNALS_AS_SYSTEM`) — CMake
  treats imported-target includes as SYSTEM by default; (d) per-source
  flags (vtz bench, benchmark version define, ninja browse.cc, cppcheck
  Clang-13 case); (e) toolchain-conditional flags (vtz `-Wshorten-64-to-32`).
- **Smallest dissolving change:** `cxx-flags`/`link-flags` on `[targets.*]`
  with the existing `{public, private}` shape, plus a profile-independent
  package-level flags block, plus the probe classifying imported interface
  includes as system (pure tool fix). Per-source flags deferred — every
  wave-1 per-source case is subsumed by codegen (B4) or was safely hoisted.

### B2. Testing story — 8/8 projects, zero test suites migrated as such

- **Hit by:** all eight. vtz (774 ctest cases hand-copied into README;
  needs args/cwd/env-set-unset/expected-nonzero-exit/fixture paths — the
  concrete `[tests]` requirements list), abseil (241 test exes + 40
  TESTONLY libs: full port declared impossible until this exists), ninja
  (gtest is now an unconditional dep of a famously zero-dep project),
  benchmark/cpptrace/cppcheck (suites dropped, mapped in GAPS), googletest
  (shell for-loop as runner), json-tui (tests always built for app-only
  consumers).
- **Three orthogonal missing pieces:** test marking on targets;
  `dev-dependencies` kept out of the export/default graph; a `cpp-pkg test`
  runner (spawn + aggregate; gtest binaries already self-report).
- **Smallest dissolving change:** the three pieces above, minimal forms.
  Case discovery (`gtest_discover_tests`) explicitly NOT needed for v1 —
  per-binary aggregation covered every wave-1 project.

### B3. Platform conditionals (cfg) — 8/8: every manifest is a macOS projection

- **Hit by:** ninja (win32/posix source split — the canonical case; also
  `USE_PPOLL`: a Linux build of this manifest silently changes runtime
  behavior), benchmark (`HAVE_*` probe answers transcribed; shlwapi/kstat/rt
  link libs), cpptrace (~8 compile-probe answers baked in), cppcheck
  (`HAVE_EXECINFO_H=1` is a macOS-only answer nothing records), abseil
  (LINKOPTS genexprs resolved at generation time; Linux build would
  silently miss `-lrt`/`-pthread`), vtz (WIN32/MSVC throughout), googletest
  and json-tui (mild).
- **Two layers, keep separate:** (1) declarative platform predicates
  (os/compiler-id) selecting sources, defines, deps, link inputs, flags —
  the 90% case, purely static; (2) probe results (has-symbol, try-compile)
  — the 10%, genuinely dynamic, overlaps codegen. Wave-1 evidence: abseil
  and benchmark prove any cfg answer must cover **link inputs and flags**,
  not just sources.
- **Smallest dissolving change:** cfg-conditional sub-tables on targets and
  deps for layer 1; document layer 2 as out of scope until a probe design
  exists (transcribed answers under a cfg key are at least *labeled* wrong
  instead of silently wrong).

### B4. Codegen escape hatch — 6/8 (unexercised only in abseil, googletest)

- **Hit by:** json-tui + cpptrace (`configure_file` version header — "the
  most common codegen pattern in the wild", both pre-generated via
  pin.sh/sed with the version now stated in three places), benchmark
  (`git describe` stamping; store strips `.git` so the fallback silently
  mis-versions rev pins — needs lockfile facts as variables, not command
  execution), ninja (browse_py.h custom command; re2c dodged), vtz (tzdb
  fetch/extract/zic + two checked-in generated headers, verify-refresh
  proved the spec in ~20 lines), cppcheck (matchcompiler: per-source
  transform over 71 files — the case where "pre-generate into patches/"
  collapses).
- **Natural tiers:** (a) template substitution with `[package]`/lockfile
  variables — no arbitrary commands, covers json-tui, cpptrace, benchmark,
  vtz's `known_zones.h`; (b) command with declared inputs/outputs emitted
  as ninja edges — covers ninja's browse, vtz's zic; (c) per-source
  transform over a glob — cppcheck only; (d) pinned asset fetch+extract —
  vtz only (`.tar.lz`!).
- **Smallest dissolving change:** tier (a) + `${pin.*}` variables; tier (b)
  next; (c)/(d) wait for more data (protobuf/ICU-class project).

### B5. Dependency patching + extractor robustness — 2/8 but contains the only blocker

- **Hit by:** abseil (**blocker**: upstream ships an `absl::strings` →
  `absl::strings` self-edge in its installed export; cpp-pkg rejects it as
  a dependency cycle, so unpatched abseil 20260526.0 cannot be consumed at
  all), vtz twice (absl 20260107.1 missing-TESTONLY broken export; date
  v3.0.4 headers in INTERFACE_SOURCES tripping the extension table).
  Workaround in all three: locally cloned+patched repos via `file://` URLs
  → per-machine commit shas → per-machine config hashes (abseil observed
  `fee068f7` vs `f4632513`), un-shareable store entries, un-committable
  lockfiles, placeholder substitution in checked-in manifests.
- **Key insight (vtz):** cpp-pkg's install-then-probe pipeline is
  *stricter* than the CPM/FetchContent ecosystem — it will keep finding
  real packaging bugs upstream never sees. That strictness is a feature
  only if users have an escape.
- **Smallest dissolving change:** three fixes, all engineering-dominated:
  (1) tool: treat self-link edges as no-ops (CMake's own semantics);
  (2) tool: skip non-compilable extensions in extracted INTERFACE_SOURCES
  (match CMake's is-compilable classification; project sources stay
  strict); (3) schema: `patches = [...]` on deps, hashed as base commit +
  patch bytes. (1)+(2) alone would have made vtz and abseil patch-free in
  wave 1, but (3) is still mandatory — the next packaging bug won't have a
  tool fix.

### B6. Install/export: producers are a cul-de-sac — 8/8, gates all library adoption

- **Hit by:** abseil ("the port is a cul-de-sac artifact-wise"), googletest
  ("if googletest adopted CppPkg.toml its ecosystem would lose
  find_package(GTest)"), benchmark (only way to consume the migration is to
  bypass it), cpptrace (upstream even vendors libdwarf.a into its prefix —
  the static-closure question needs explicit design), vtz (FILE_SET
  HEADERS api/impl split maps directly onto a `public-headers` field),
  ninja + json-tui (single-binary `install` verb), cppcheck (**runtime
  data**: binary silently reports zero findings until cfg/*.cfg is staged;
  also FILESDIR = install-prefix path fed back into compilation as a
  define).
- **Smallest dissolving change:** point the existing dependency shim
  emitter at the project's *own* manifest (`cppkg-manifest.json` + Config
  shim from local `[targets]`) — the round-trip fixpoint. Runtime-data
  staging (`runtime-data` globs copied next to the output) is separable and
  fixes cppcheck's silent-broken shape today.

### B7. System deps + pseudo-package dedup + hermeticity — 6/8, one silent leak

- **Hit by:** Threads::Threads ambiguous-ownership dance in vtz (4 dep
  configs), json-tui, ninja, googletest, benchmark, abseil — the error
  message is praised in three GAPS files, but the model demands an owner
  for a find-module target nobody owns. cpptrace found the sharp version:
  Homebrew `libzstd.dylib` absolute path baked into a store manifest with
  no warning and no hash contribution — machine-dependent artifacts under
  identical keys. cppcheck (ambient Boost auto-detected; system PCRE has no
  CMake config to probe at all), benchmark/abseil (`-lrt`, `-pthread`
  have nowhere to go on Linux).
- **Smallest dissolving change:** (1) builtin list of well-known find-module
  pseudo-packages (Threads first) deduped as shared-and-identical — removes
  a workaround line from nearly every multi-dep migration; (2) hermeticity
  scan: warn/error on undeclared absolute paths escaping into store
  manifests; (3) a system-dependency declaration form (design needed —
  interacts with the config hash).

### B8. Glob exclusion — 4/8, tiny fix, real drift hazard

- **Hit by:** cppcheck (`cli/*.cpp` minus `main.cpp` — the cli *library*
  could not be reproduced), benchmark (19 files hand-listed; a new upstream
  file becomes a silent link error), vtz (7-file bench list), ninja
  (explicitly notes exclusion would NOT help its win32 split — that's B3).
- **Smallest dissolving change:** negative patterns
  (`"!src/benchmark_main.cc"`) or an `exclude` key. Pure ergonomics,
  engineering-dominated, no interaction with anything else.

### B9. Package-level defaults / target-defaults — 5/8, 29% of abseil's TOML

- **Hit by:** abseil (two lines repeated identically in 93 targets = 29%
  of 660 generated lines), cppcheck (4 defines + `cxx-std` on all 5
  targets, "the single ugliest thing in the manifest", drift hazard),
  googletest (`cxx-std = 17` × 14; forgetting one silently builds at
  toolchain default), benchmark, json-tui (directory-scoped
  add_definitions).
- **Smallest dissolving change:** a `[target-defaults]` table targets
  inherit (cxx-std, defines, includes, flags once B1 lands). Related but
  separable: target templating for ninja's 7 unrolled perftests / vtz's 5
  examples, and abseil's `targets-from` include wish — defer both; defaults
  alone fix the measured 29%.

### B10. interface-library kind — 3/8 direct, structural for Boost

- **Hit by:** abseil (54 of 93 targets are header-only, faked as static
  libs over an empty stub TU — "the single schema addition that removes the
  biggest hack in this port"), cppcheck (picojson degrades into consumers'
  private include dirs, losing include hygiene), vtz (vendored ankerl —
  dodged by using the real package).
- **Smallest dissolving change:** un-defer the already-reserved
  `interface-library` kind + `interface` visibility bucket. Propagation
  semantics already exist (extraction handles INTERFACE imported targets).

### B11. object-library / whole-archive — 4/8, worst *failure shape*, no wave-1 casualty

- **Hit by:** vtz (only real cost: 9 TUs compiled twice + latent ODR trap),
  ninja + cppcheck (archives happened to be equivalent; both GAPS warn the
  static-initializer-registration pattern would *silently drop code* — no
  error, just missing behavior), abseil (unexercised outside DLL mode).
- **Smallest dissolving change:** per-edge `whole-archive` attribute in the
  reserved `{ target = ... }` table form (fixes the silent-wrong hazard);
  vtz's actual need is better served by "one target, multiple exported
  interface views" than by CMake-style OBJECT libs — needs a design
  discussion, not a rushed kind.

### B12. Fetch-layer + probe papercuts — batch of cheap, evidenced fixes

Each hit 1–5 projects; all are hours-level tool changes:
- `find-package` field: **document it** (works great — ninja, json-tui,
  googletest; absent from CPPKG_TOML.md) and translate the probe's raw
  CMake config-not-found error into the hint (vtz, abseil, googletest hit
  the raw error).
- `subdir` field on git/url deps, folded into the config hash (cpptrace:
  zstd's CMakeLists lives in `build/cmake/`; **no workaround exists**, a
  feature was dropped).
- Submodule guard: trigger on actual gitlink entries, not `.gitmodules`
  file presence (json-tui: 0-byte file false positive).
- url deps: accept `.tar.xz`/`.tar.bz2` (json-tui; system tar already does
  it).
- Evaluate `$<BOOL:...>` conditionals inside LINK_ONLY instead of skipping
  (vtz, abseil: correct-by-accident on macOS, silently drops `-lrt` on
  Linux; also stop replaying the notes on cache hits).
- Translate the CMake-≥4 `cmake_minimum_required` refusal into a
  `CMAKE_POLICY_VERSION_MINIMUM` hint (json-tui; will hit every old pin).
- Misc: per-config build dirs (cpptrace), lint unknown dep `options` keys
  (cpptrace), document flag-ordering last-wins as contract (cppcheck).

---

## 3. DESIGN SKETCHES — top 5

> **DRAFT FOR USER TASTE REVIEW.** Candidates are concrete but winners are
> only picked where engineering dominates; contested aesthetics are marked
> and left open. Bar: clean, declarative, cargo-familiar yet sufficient for
> C++ natives; escape hatches minimal but sufficient.

### S1. Per-target flags (B1)

**Candidate A — mirror the `defines` shape (visibility split):**

```toml
[targets.json-tui-lib]
cxx-flags  = { private = ["-Wall", "-Wextra", "-Werror"], public = ["-fno-exceptions"] }
link-flags = { private = ["-Wl,-framework,CoreFoundation"] }

[flags]                      # profile-independent, all targets, all profiles
cxx-flags = ["-Wno-deprecated"]
```

Pro: `-fno-exceptions` PUBLIC (json-tui) is literally a propagating compile
requirement — the visibility model already exists and users already know it;
kills the 4× profile duplication (ninja, benchmark, cpptrace). Con: public
flags propagate arbitrary strings across the graph — footgun surface
(`-O3` public would be legal and terrible).

**Candidate B — private-only flags + named propagating knobs:**

```toml
[targets.json-tui-lib]
cxx-flags   = ["-Wall", "-Wextra", "-Werror"]   # always private
link-flags  = ["-Wl,-framework,CoreFoundation"]
exceptions  = false                              # public by definition, curated
```

Pro: propagation is restricted to a curated, ABI-meaningful vocabulary
(`exceptions`, `rtti`, later `lto`) — no public-flag footgun; flat list
reads cleaner for the 95% private case. Con: the vocabulary chases reality
forever; the first un-curated public flag forces Candidate A anyway.

**Candidate C — A's shape plus bare-list sugar** (bare list ≡ all-private,
matching the existing deps sugar):

```toml
cxx-flags = ["-Wall", "-Wextra"]                          # sugar: private
cxx-flags = { private = [...], public = ["-fno-exceptions"] }  # full form
```

**Recommendation (engineering dominates in part):** the `-isystem` fix is
not optional and not schema — the probe must classify imported-target
interface includes as system (json-tui's break, CMake-matching). Bare-list
sugar (C) is consistent with existing schema grammar. Whether public flags
are open strings (A/C) or a curated vocabulary (B) is **taste — user call**;
wave-1 evidence needs exactly one public flag (`-fno-exceptions`), which
weakly favors B's discipline but A's generality.

### S2. Testing story (B2)

**Candidate A — test as a target type:**

```toml
[dev-dependencies]
googletest = { git = "...", tag = "v1.17.0", find-package = "GTest" }

[targets.test-vtz]
type    = "test"                       # implies: dev-deps visible, excluded
sources = ["etc/test/impl/*.cpp"]      # from default build, run by `cpp-pkg test`
dependencies = ["vtz-impl", "GTest::gtest_main"]
args = ["--build", ".", "--testdata", "../etc/testdata"]
cwd  = "tzdb-runtime"
env  = { VTZ_TZDATA_PATH = "..." }
env-unset = ["TZ"]
expect-failure = false                 # vtz death tests: true
```

Pro: cargo-familiar (`type = "test"` reads like `[[test]]`); one keyword
buys marking + dev-dep visibility + default-build exclusion. Con: a target
that is both a benchmark and a test (benchmark's output-checked tests)
needs two stanzas.

**Candidate B — orthogonal marker + run table:**

```toml
[targets.test-vtz]
type = "executable"
test = true                            # marking only

[targets.test-vtz.run]                 # how `cpp-pkg test` invokes it
args = [...]; cwd = "..."; env = {...}; expect-failure = true
```

Pro: any executable can be a test; run-config separated from build-config.
Con: two knobs where A has one; `test = true` + `type = "executable"` is
boilerplate for the common case.

**Candidate C — multiple invocations per binary** (vtz runs `test_vtz_api`
twice, with and without `--no_set_install`):

```toml
[[targets.test-vtz-api.runs]]
args = []
[[targets.test-vtz-api.runs]]
args = ["--no_set_install"]
```

**Recommendation (engineering dominates):** `[dev-dependencies]` exactly as
spelled — 8/8 projects need it, zero design freedom, Cargo-proven, and it
alone un-poisons ninja/json-tui/googletest. Runner v1 = build marked
targets, spawn with declared args/cwd/env, aggregate exit codes — vtz's 774
tests prove that surface is sufficient (no case discovery needed).
A-vs-B marking is **taste — user call**; C's repeated-run form should exist
in whichever shape wins (vtz needs it on day one).

### S3. Platform conditionals (B3)

**Candidate A — cargo-style cfg sub-tables (merge semantics):**

```toml
[targets.libninja]
sources = ["src/build.cc", "..."]                # 27 common sources

[targets.libninja.cfg.windows]
sources = ["src/subprocess-win32.cc", "src/getopt.c", "..."]
defines = { private = ["NOMINMAX"] }

[targets.libninja.cfg.unix]
sources = ["src/subprocess-posix.cc", "src/jobserver-posix.cc"]

[dependencies.zlib.cfg.linux]                    # deps and link inputs too
# ...
```

Pro: declarative, greppable, additive merge is easy to specify; the
predicate is a table key so the grammar stays TOML-native. Con: predicate
algebra (`windows`, `macos`, `linux`, `unix`, compiler-id? and/or/not?)
needs deciding; deep nesting for one-entry differences.

**Candidate B — `when` on entry tables (inline, per-entry):**

```toml
sources = [
  "src/build.cc",
  { path = "src/subprocess-win32.cc", when = "windows" },
  { path = "src/subprocess-posix.cc", when = "unix" },
]
link-flags = [{ flag = "-lrt", when = "linux" }]
```

Pro: variation lives next to what varies — ninja's split reads as one
annotated list; extends string-or-table precedent already reserved for dep
entries. Con: noisy at scale (7 win32 entries × table form); mixing sugar
strings and tables in one list is the least "minimal" part of the current
grammar.

**Candidate C — genexpr-lite condition strings** (`"$if(windows): ..."`).
Rejected outright: un-declarative, un-greppable, imports CMake's worst
idea. Listed only for completeness.

**Recommendation (engineering dominates in part):** whichever surface wins,
the v1 predicate vocabulary must be closed and tiny — `os` (windows / macos
/ linux / unix) and `compiler` (clang / gcc / msvc) — because that covers
every wave-1 instance except the probes; probe-shaped predicates
(has-symbol, try-compile) are **explicitly out** and routed to B4/future
(benchmark GAPS: "the moral equivalent of build.rs"). Conditionals must
apply to sources, defines, deps, link inputs, and flags (abseil evidence).
A-vs-B is **taste — user call**; A composes better with `[target-defaults]`
(B9), B reads better for single-file splits.

### S4. Codegen escape hatch (B4)

**Candidate A — two tiers, no per-source transform yet:**

```toml
# Tier 1: template substitution — no commands, pure function of manifest+lockfile
[generate.version-header]
template = "src/version.hpp.in"                  # @VAR@ substitution
output   = "src/version.hpp"                     # lands under build/gen/
vars     = { PROJECT_VERSION = "${package.version}",
             BENCHMARK_VERSION = "${pin.self.tag}" }   # lockfile facts

# Tier 2: declared command — emitted as a ninja edge, inputs hashed
[generate.browse-py-h]
command = ["sh", "src/inline.sh", "browse_py_h"]
stdin   = "src/browse.py"
inputs  = ["src/browse.py", "src/inline.sh"]
outputs = ["build/browse_py.h"]

[targets.ninja]
includes = { private = ["${gen}"] }              # generated root referenceable
```

Pro: tier 1 alone covers json-tui, cpptrace, benchmark, and vtz's
`known_zones.h` with zero arbitrary execution; tier 2 is ordinary ninja
incrementality (fixes pin.sh staleness); `${pin.*}` solves benchmark's
store-strips-.git trap by construction. Con: two mechanisms; `${var}`
interpolation is a new grammar element that must stay tightly scoped.

**Candidate B — one general command form only** (tier 1 expressed as a
`cpp-pkg`-provided substitute subcommand). Pro: single mechanism. Con: the
most common case (version headers) now shells out for what is a pure
function; hermeticity analysis of commands is undecidable, of templates
trivial.

**Candidate C — tier 1 only in v1.** Pro: minimal. Con: ninja's browse and
vtz's zic remain in pin.sh with silent-staleness; ninja GAPS explicitly
sized the `[generate]` table as covering both of its instances.

**Recommendation (engineering dominates):** Candidate A. Template tier is
hermetic by construction; command tier requires declared inputs/outputs and
outputs confined to `${gen}` (refusing source-tree writes — vtz GAPS calls
source-tree generation the pattern to reject, checked-in-generated + a
`verify` mode the pattern to bless). Defer cppcheck's per-source transform
(unique in wave 1) and vtz's `[assets]` fetch (unique, and half-solved by
its test-fixture nature → B2's fixture provisioning). Exact spelling of
interpolation (`${pin.self.tag}` vs `version-from = "git-tag"`) is **taste
— user call**.

### S5. Dependency patches (B5)

**Candidate A — patch-file list:**

```toml
[dependencies.absl]
git     = "https://github.com/abseil/abseil-cpp"
tag     = "20260526.0"
patches = ["patches/0001-remove-strings-self-dep.patch"]
# config hash = base commit + blake3 of patch bytes, in order
```

Pro: Conan/Nix/Buildroot precedent; patch bytes live in the consumer's VCS
so the lockfile and store keys are machine-independent (fixes the observed
`fee068f7` vs `f4632513` split); reviewable diffs. Con: patch application
semantics must be pinned (strip level, fuzz = 0, fail-on-offset?).

**Candidate B — overlay directory** (`overlay = "deps/absl-overlay/"`,
files copied over the checkout, dir content-hashed). Pro: no patch-format
semantics; trivially deterministic. Con: whole-file replacement hides the
actual change; merges terribly across dep version bumps — the LTS-bump
maintenance story (abseil GAPS) is exactly where overlays rot.

**Candidate C — first-class fork pinning only** (status quo, documented).
Rejected by wave-1 evidence: `file://` clones produced per-machine hashes,
un-committable lockfiles, and placeholder substitution in a checked-in
manifest (vtz).

**Recommendation (engineering dominates):** Candidate A, `-p1` strict,
zero-fuzz, applied before configure, hashed as base + ordered patch bytes.
Ship together with the two tool fixes that remove the wave-1 *need* for
patches (self-edge = no-op; skip non-compilable INTERFACE_SOURCES) — the
field is the escape hatch, the fixes keep it rarely used. No taste question
here worth blocking on.

---

## 4. WAVE-2 READINESS

### Apache Arrow (app-scale consumer with a deep dep tree)

Arrow is wave-1's dep-provisioning gaps at 10× scale: dozens of third-party
deps (thrift, orc, re2, rapidjson, zstd, lz4, snappy...), bundled-vs-system
toggles for each, heavy optional-component structure, configure-time
detection everywhere.

**Must land (blocking):**
- B5 complete: `patches` field + self-edge fix + INTERFACE_SOURCES fix — at
  Arrow's dep count, hitting ≥1 broken installed export is near-certain
  (wave 1 hit 3 in 14 total deps).
- B12 `subdir` field — zstd is *already* on Arrow's dep list and already
  proven undeclarable (cpptrace).
- B7 (1)+(2): pseudo-package dedup + hermeticity scan — with ~20 dep
  configs each running `find_dependency(Threads)`, the arbitrary-ownership
  dance stops scaling, and ambient Homebrew leakage (observed with zstd on
  this exact machine) becomes probable rather than possible.
- B3 cfg conditionals — Arrow's own defines/link inputs are heavily
  platform-conditional; a macOS-projection manifest of Arrow would be too
  large to hand-re-derive per platform.
- B12 fetch formats (`.tar.xz`) and the CMAKE_POLICY_VERSION_MINIMUM hint —
  old pins guaranteed in Arrow's third-party toolchain.

**Strongly wanted (severe friction without):** B1 per-target flags (vendored
warning relaxation), B2 dev-dependencies (Arrow's gtest/gbench footprint),
B4 tier 1 (Arrow generates version headers), B7 (3) system-dep declaration
(Arrow's system-vs-bundled toggles are its central build idiom — expect
this to be Arrow's headline gap and plan the migration to produce design
data for it rather than pre-building it).

### Boost (producer-scale library collection)

Boost is wave-1's abseil findings at 30× scale: ~150 libraries, majority
header-only, plus compiled outliers with exotic needs.

**Must land (blocking):**
- B10 `interface-library` — abseil needed 54 stub archives for 93 targets;
  Boost would need thousands. The stub hack does not scale past wave 1.
- B9 `[target-defaults]` — 29% repetition measured at 93 targets makes a
  Boost-scale manifest unreviewable even when generated.
- B6 install/export — abseil-the-port was already "a cul-de-sac
  artifact-wise"; a Boost port with no consumption path is strictly
  pointless, since Boost exists only to be consumed. This is the gap where
  Boost differs from Arrow: producer story first.
- B2 dev-dependencies + runner — Boost.Test-based suites are the only
  correctness signal for most Boost libs.
- Generator tooling as sanctioned pattern (abseil's verdict: generator AND
  sugar, different roles) — Boost.CMake structure is regular enough to
  mine, but expect `targets-from`-style file inclusion (B9-adjacent,
  currently deferred) to graduate to required at ~1,000+ targets.

**Known unknown, flagged now:** Boost.Context ships assembly sources
(`.S`/`.asm`) — outside the current hard extension table entirely (C++/C
only). No wave-1 project touched assembly; treat "extension table meets
asm" as a new gap class to be scoped *before* committing to Boost, or scope
wave-2 Boost to a Context-free subset (headers + a compiled lib like
Boost.Filesystem or Boost.Regex).

**Sequencing implication:** Arrow and Boost stress disjoint backlog halves
(Arrow → B5/B7/B3/B12; Boost → B10/B9/B6/B2). Landing B1+B2 (shared) plus
each track's blockers allows the two wave-2 migrations to proceed in
parallel without contending for the same tool work.
