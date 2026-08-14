# B2 Testing story — design candidates

Area: **testing** (test marking on targets, dev-dependencies, `cpp-pkg test`
runner surface). Explicitly out of scope: per-case discovery
(`gtest_discover_tests`-style), output-matching harnesses, coverage.
Sources: BACKLOG.md B2 + S2; GAPS.md testing-story sections of all eight
wave-1 migrations; CPPKG_TOML.md (v0 normative schema). Status: candidates
for the taste judge; a recommendation is stated but not binding.

---

## 0. Requirements distilled from the corpus

Every requirement below traces to a wave-1 observation; nothing here is
speculative.

**R1 — Test marking must cover libraries, not just executables.** vtz's
`vtz_testing` is a *static library* of test-support code (depends on
GTest/date); abseil has **40 TESTONLY libraries** alongside its 241 test
executables. Any design whose marking applies only to runnable targets
leaves the support-library half of the problem unsolved.

**R2 — Dev-dependencies out of the default and export graphs.** json-tui:
googletest builds for someone who only wants the app (GAPS §5). ninja:
gtest became an unconditional dep of a famously zero-dep project (GAPS §3).
googletest mode (b): gtest poisons a library's public dep set (GAPS §2).
benchmark: "declaring gtest would poison the production dependency closure"
(GAPS §5). 8/8 projects need this; BACKLOG calls the shape
(`[dev-dependencies]`, Cargo-spelled) engineering-decided.

**R3 — Runner surface, from vtz's 774-case parity run** (the concrete
requirements list; README "Reproduce" section):
- per-invocation **args** (`--build . --testdata ../etc/testdata`),
- **cwd** (`tzdb-runtime/`, a fixture directory),
- **env set** (`VTZ_TZDATA_PATH=...`) and **env unset** (upstream's
  `ENVIRONMENT_MODIFICATION` unset semantics — set-to-empty ≠ unset),
- **expected nonzero exit** (2 death tests),
- **multiple invocations of one binary** (`test_vtz_api` runs twice, with
  and without `--no_set_install`; `test_tzdb_load` runs three times).
- benchmark adds: args like `--benchmark_min_time=0.01` (~40 CTest
  registrations); ninja adds: run from a writable scratch cwd.

**R4 — Per-binary aggregation is sufficient.** gtest binaries self-report
and exit nonzero on failure (googletest GAPS §1); vtz's 774 cases were fully
verified through 6 invocations. Case discovery is explicitly not needed
for v1 (BACKLOG B2).

**R5 — `cpp-pkg build` default set.** Today `cpp-pkg build` builds **all**
`[targets]` (`graph::plan(only: &[])` = all; cli.rs). The design must state
what the default becomes and keep unmarked manifests behaving identically
(taste charter: features cost nothing when unused).

---

## 1. Common substrate (shared by all three candidates)

The candidates differ only in **how test-ness is spelled and where run
configuration lives**. Everything in this section is common, and per
BACKLOG S2 is engineering-dominated ("`[dev-dependencies]` exactly as
spelled — zero design freedom").

### 1.1 `[dev-dependencies]`

```toml
[dev-dependencies]
googletest = { git = "https://github.com/google/googletest", tag = "v1.17.0", find-package = "GTest" }

[dev-dependencies.date]
git     = "https://github.com/HowardHinnant/date"
tag     = "v3.0.4"
options = { BUILD_TZ_LIB = "ON", MANUAL_TZ_DB = "ON" }
```

**Grammar:** identical to `[dependencies]` in every field (`git`+`tag|rev`,
`url`+`sha256`, `options`, `needs`, `find-package`, `exposes-namespace`,
`exposes-targets`, and future B5 `patches`). One `DependencySpec`, two
tables. No new dependency grammar to learn — this is the whole point.

**Semantics:**

- **Single resolution namespace.** Exported target names from dev-deps
  enter the same naming ladder (CPPKG_TOML.md "Target references"). Name
  resolution runs first, unchanged; a *visibility check* runs after:
  only dev targets (per-candidate marking, §2–§4) may reference a target
  owned by a dev-dependency. Violation is a resolve-time hard error:

  ```
  error: target 'json-tui' (not a dev target) depends on 'GTest::gtest_main',
         exported by dev-dependency 'googletest'
  hint: mark the target dev/test, or move 'googletest' to [dependencies]
  ```

- **Key collision:** the same key in `[dependencies]` and
  `[dev-dependencies]` is a hard error at load time.
- **`needs` direction:** a regular dependency's `needs` may not name a
  dev-dependency (its Config would `find_dependency` something consumers
  don't have) — hard error. A dev-dependency's `needs` may name either
  table; the `CMAKE_PREFIX_PATH` closure rule (CPPKG_TOML.md) is unchanged.
- **Laziness:** dev-deps are fetched/built/probed only when the requested
  build set contains ≥1 dev target whose closure reaches them. Plain
  `cpp-pkg build` performs **no network or store activity** for dev-deps —
  json-tui's app-only consumer never touches googletest, not even a
  `ls-remote`. Consequence: dev-dep lockfile entries appear on the first
  `cpp-pkg test` / explicit dev-target build, not on first `cpp-pkg build`
  (flagged in Open Questions — Cargo eagerly locks everything).
- **Lockfile:** entries use the existing `[[package]]` grammar unchanged.
  No format change; the two tables are a manifest-side distinction only.
- **Config hash / store:** unchanged machinery. A dev-dep builds, hashes,
  and caches exactly like a regular dep.
- **Export (B6 interaction):** when the producer story lands, emitted
  package manifests / Config shims exclude dev targets and dev-deps
  unconditionally. This is the "out of export graph" half of R2 and is a
  one-line policy in the emitter.

### 1.2 The `cpp-pkg test` runner

```
cpp-pkg test [FILTER...] [--config <cfg>] [--toolchain <t>] [--jobs N]
             [--list] [--verbose] [-- PASSTHROUGH...]
```

- **Selection:** no FILTER = every test target. FILTERs match test-target
  names exactly, or with `*` globs (`test_*`). A FILTER matching nothing is
  a hard error (CI typo safety). No FILTER and zero test targets in the
  manifest: prints "no test targets" and exits 0 (Cargo behavior).
- **Build:** runs the ordinary pipeline (cli.rs steps 1–6) with the
  requested set = selected test targets; dev-deps in their closures are
  provisioned on demand. Same `--config`/`--toolchain` surface as `build`;
  profiles apply to test targets like any other target (so
  `cpp-pkg test --config debug` with an ASan profile works today, with the
  existing "dependencies are uninstrumented" warning).
- **Invocations:** each test target expands to its declared run entries
  (per-candidate spelling), or to **one default invocation** if none are
  declared: no args, cwd = project root, inherited env, expect exit 0.
  This makes the 90% case (googletest's 10 samples, json-tui, ninja)
  zero-config.
- **Execution:** spawn directly (argv array, **no shell**), serial by
  default (`--jobs N` opt-in — vtz's tests share a fixture cwd; CTest is
  also serial by default). stdout/stderr captured, replayed on failure;
  `--verbose` streams. PASSTHROUGH args after `--` are appended to every
  selected invocation (`cpp-pkg test test_vtz -- --gtest_filter=Zone.*` —
  the googletest GAPS ask).
- **Pass criterion:** exit status 0 ⇒ pass. With `expect-failure = true`:
  pass iff the process exited nonzero **or died by signal** — vtz's death
  tests abort (SIGABRT), and upstream's own check is the shell's
  `test $? -ne 0`, which sees 128+N for signal deaths. (CTest's `WILL_FAIL`
  treats crashes as failures regardless; we deliberately match the
  weaker/simpler shell semantics and note it — Open Questions.)
- **cwd rule:** relative, resolved against the project root. If the
  resolved cwd lies **inside the build directory**, the runner creates it
  (ninja's "writable scratch cwd"); otherwise it must already exist
  (vtz's `tzdb-runtime/` fixture tree) — a missing fixture cwd is reported
  as that invocation failing with a clear reason, and the suite continues.
- **env rule:** start from the inherited environment, apply `env-remove`
  (unset), then `env` (set). Setting to `""` and removing are distinct —
  exactly the CMake `ENVIRONMENT_MODIFICATION` distinction vtz's
  standalone tests exercise.
- **Interpolation:** args, env values, and cwd accept exactly two
  variables: `${project-root}` and `${build-dir}` (absolute paths). Same
  `${...}` grammar as S4's `[generate]` `vars`/`${gen}` — one resolver
  module, disjoint namespaces (interaction, §6.3). Unknown variable =
  load-time hard error.
- **Reporting:** one line per invocation
  (`PASS test_vtz_api (2/2: default, embedded)` style), summary
  `N passed, M failed`, exit 0 iff all passed. `--list` prints the
  expanded invocation table without building or running.
- **Default build set change (R5):** `cpp-pkg build` builds all
  **non-dev** targets (plus whatever their closures reach). Dev targets
  build when named explicitly (`cpp-pkg build test_vtz` works and
  provisions dev-deps) or via `cpp-pkg test`. A manifest with no marks has
  no dev targets — behavior is byte-identical to v0. Support libraries like
  `vtz_testing` would already be skipped by reachability once tests are
  excluded, but they still need marking for the *visibility* check (R1) —
  reachability-inferred dev-ness was considered and rejected: it makes
  "may this target see gtest?" a whole-graph property where adding one
  edge flips legality at a distance; explicit marking gives local errors
  (Bazel's `testonly` precedent).

---

## 2. Candidate T1 — test as a target kind

The S2-A sketch, refined. Test-ness is a **kind**; the corpus forces one
addition the sketch missed: a `dev` attribute for non-runnable dev targets
(R1 — `vtz_testing`, abseil's 40 TESTONLY libs, `bench_vtz`).

### 2.1 Surface

```toml
[targets.test-expander]
type    = "test"                      # an executable in the dev graph,
sources = ["src/expander_test.cpp"]   # run by `cpp-pkg test`
dependencies = ["json-tui-lib", "GTest::gtest_main"]

[targets.vtz_testing]
type = "static-library"
dev  = true                           # dev graph, not runnable

[targets.bench_vtz]
type = "executable"
dev  = true                           # dev graph, not a test

[[targets.test-expander.run]]         # optional; absent = default invocation
args = ["--gtest_brief=1"]
```

### 2.2 Semantics

- `type = "test"`: executable in every build-mechanical respect (sources,
  link language, flags); member of the dev graph (may see dev-deps,
  excluded from default build, excluded from export); registered with the
  runner.
- `dev = true` on `executable`/`static-library`: dev graph membership
  only. `dev = true` on a `type = "test"` target: redundant, warning.
  `dev = false` on `type = "test"`: hard error (the kind *is* dev-ness).
- `[[targets.X.run]]` entries (fields per §1.2: `name`, `args`, `cwd`,
  `env`, `env-remove`, `expect-failure`) are legal only on `type = "test"`
  targets; elsewhere a hard error naming the fix.
- Duplicate `run` `name`s within a target: error. Unnamed entries report
  as `target#1`, `target#2`.

### 2.3 Edge cases / errors

- A `type = "test"` target that another *shipped* target depends on: error
  (nothing may depend on an executable today; unchanged).
- A non-dev target depending on a `dev = true` library: the §1.1
  visibility error, same shape.
- Dual-role targets (benchmark's output-checked "tests that are also the
  smoke bench", a shipped binary you want smoke-run): **inexpressible** —
  a target is either `type = "test"` (not shipped) or not a test. You
  duplicate the stanza. This is T1's structural cost.

### 2.4 Costs

- Two spellings of dev-ness (`type = "test"` implies it; `dev = true`
  states it) — the kind looks like the whole story but isn't; every
  project with a test-support lib or bench needs both.
- Kind now bundles three orthogonal facts (executable, dev, runnable) —
  against charter tie-breaker (3).
- Pro: the common case is one greppable line; reads like Cargo's
  auto-registered `tests/`; `cpp-pkg test` semantics discoverable from the
  manifest instantly.

---

## 3. Candidate T2 — orthogonal markers + per-target run entries (recommended)

The S2-B sketch, refined into two booleans with a strict implication, plus
S2-C's repeated-run form.

### 3.1 Surface

```toml
[targets.tests]
type = "executable"
test = true                      # implies dev = true
sources = ["src/expander_test.cpp"]
dependencies = ["json-tui-lib", "GTest::gtest_main"]
# no [[run]] table: one default invocation (no args, project-root cwd)

[targets.vtz_testing]
type = "static-library"
dev  = true                      # dev graph; not runnable, so not `test`

[targets.bench_vtz]
type = "executable"
dev  = true                      # dev graph, deliberately not a test

[[targets.test_vtz_api.run]]     # N entries = N invocations (vtz needs 2–3)
name = "embedded"
cwd  = "tzdb-runtime"
args = ["--no_set_install", "--build", ".", "--testdata", "../etc/testdata"]
env  = { VTZ_TZDATA_PATH = "${project-root}/tzdb-runtime/data/tzdata" }
env-remove     = ["TZ"]
expect-failure = false
```

### 3.2 Semantics

- `dev = true` (any target kind): member of the dev graph — may reference
  dev-dep targets and other dev targets; excluded from the default build
  set and from export; buildable by explicit name; ordinary in every other
  respect (flags, profiles, cfg when S3 lands).
- `test = true` (executables only; on a `static-library` it is a hard
  error with hint "libraries use `dev = true`"): implies `dev = true`, and
  registers the target with `cpp-pkg test`. Writing both is legal and
  redundant. `test = true, dev = false` is a hard error in v1 (reserved:
  if the taste judge wants shipped-and-smoke-tested binaries, lifting this
  to "ships AND is run by the runner" is additive — Open Questions).
- Edge rule (the whole model in one sentence): **a non-dev target may not
  depend on a dev target or a dev-dep-owned target; every other direction
  is legal.** Locally checkable, one error message.
- `[[targets.X.run]]`: legal only when `test = true`. Fields exactly §1.2:
  `name` (optional, unique within target), `args` (list of strings),
  `cwd` (string), `env` (string→string table), `env-remove` (list),
  `expect-failure` (bool, default false). All optional; `deny_unknown_
  fields` strict like the rest of schema.rs. Zero entries = one default
  invocation; N entries = exactly N invocations, declared order.

### 3.3 Edge cases / errors

- `run` on a non-test target → error ("run entries require test = true").
- `test = true` + `type = "static-library"` → error (above).
- Filters (`cpp-pkg test FILTER`) match target names; `--list` shows the
  expansion including run names.
- A dev target named on `cpp-pkg build` builds but does not run.
- Marker keys are booleans with defaults `false`; TargetSpec additions are
  `#[serde(default)]` so v0 manifests parse unchanged.

### 3.4 Costs

- The common case is two lines (`type = "executable"` + `test = true`)
  where T1 spends one.
- Two markers to document, one implication to teach ("test implies dev").
- Dual-role binaries are excluded in v1 (same as T1, but here the door is
  explicitly reserved rather than structurally shut).
- Pro: one orthogonal dev axis covers executables, support libraries and
  benches uniformly (R1 exactly — `dev = true` is Bazel's `testonly`, and
  abseil's generator emits it 1:1 from `TESTONLY`); run-config lives next
  to the target it configures; the declarative reading never lies
  (`bench_vtz` is not labeled a test, because it isn't one).

---

## 4. Candidate T3 — top-level `[[tests]]` invocation registry

CTest's mental model, made declarative: targets are pure build objects;
tests are *invocations* declared in their own top-level list.

### 4.1 Surface

```toml
[targets.test_vtz_api]
type = "executable"
dev  = true                          # dev-graph membership stays explicit
sources = ["etc/test/test_api/*.cpp"]
dependencies = ["vtz_testing", "vtz"]

[[tests]]
target = "test_vtz_api"
cwd    = "tzdb-runtime"
args   = ["--build", ".", "--testdata", "../etc/testdata"]

[[tests]]
name   = "api-embedded"
target = "test_vtz_api"
cwd    = "tzdb-runtime"
args   = ["--no_set_install", "--build", ".", "--testdata", "../etc/testdata"]
```

### 4.2 Semantics

- `[[tests]]` entries: `target` (required, must name a local executable
  target), plus the §1.2 run fields. `cpp-pkg test` = build the closure of
  all referenced targets, run all entries in declaration order.
- Test-ness is *being referenced*: no `test` marker exists. Dev-graph
  membership remains the explicit `dev = true` attribute (visibility can't
  be inferred from `[[tests]]` without whole-graph spookiness; see §1.2).
- A `[[tests]]` entry may reference a **non-dev** target — a shipped
  binary can be smoke-tested without leaving the default build. This is
  the one thing T3 expresses that T1/T2 (v1) cannot.
- Filters match `name` (default: target name) with globs.

### 4.3 Edge cases / errors

- `target` naming a library or an unknown target: hard error.
- Duplicate `name`s: error.
- A dev executable referenced by no `[[tests]]` entry: legal (a bench).
- The hazard: a test target whose author forgets `dev = true` — not
  silent (its gtest reference trips the §1.1 visibility error), but the
  error arrives pointing at the dependency edge, and the fix ("add
  dev = true *and* keep your [[tests]] entry") touches a different table
  than the one the user was editing.

### 4.4 Costs

- The common case pays the most: googletest's 10 samples = 10 `dev = true`
  marks **plus** 10 `[[tests]]` stanzas (T2: 10 one-word marks). abseil:
  241 registry entries on top of 241 marked targets.
- "Which targets are tests" requires scanning a second table; the target
  stanza no longer tells you.
- Two coupled declarations per test = drift surface (rename a target,
  forget the registry).
- Pro: multiple invocations are the native shape rather than a sub-array;
  run config and build config fully separated; C++ natives recognize
  `add_test` immediately; dual-role/smoke-testing shipped binaries falls
  out for free.

---

## 5. The corpus, before and after

After-examples use T2 spelling; T1 differs by `type = "test"` replacing
`type = "executable"` + `test = true`; T3 moves each `[[targets.X.run]]`
to `[[tests]] target = "X"` and keeps `dev = true` on the target.

### 5.1 json-tui (the mandated evidence: consumers don't pay for tests)

Before (migrations/json-tui/CppPkg.toml:78–94, 131–139 — workaround
comments included upstream's `JSON_TUI_BUILD_TESTS=ON` gate being lost):

```toml
# Test-only dependency; upstream pins this exact commit in cmake/test.cmake.
# GAP(testing-story): cpp-pkg has no notion of test-only deps — googletest is
# fetched/built even when only the app targets are wanted.
[dependencies.googletest]
git = "https://github.com/google/googletest"
rev = "23ef29555ef4789f555f1ba8c51b4c52975f0907"
find-package = "GTest"
[dependencies.googletest.options]
BUILD_GMOCK = "OFF"
CMAKE_POLICY_VERSION_MINIMUM = "3.5"

# Upstream builds this only when -DJSON_TUI_BUILD_TESTS=ON.
# GAP(conditional-sources / testing-story): ... run by hand.
[targets.tests]
type = "executable"
sources = ["src/expander_test.cpp"]
cxx-std = 20
includes = { private = ["src"] }
dependencies = ["json-tui-lib", "GTest::gtest_main"]
```

After:

```toml
[dev-dependencies.googletest]
git = "https://github.com/google/googletest"
rev = "23ef29555ef4789f555f1ba8c51b4c52975f0907"
find-package = "GTest"
[dev-dependencies.googletest.options]
BUILD_GMOCK = "OFF"
CMAKE_POLICY_VERSION_MINIMUM = "3.5"

[targets.tests]
type = "executable"
test = true
sources = ["src/expander_test.cpp"]
cxx-std = 20
includes = { private = ["src"] }
dependencies = ["json-tui-lib", "GTest::gtest_main"]
```

`cpp-pkg build` now builds `json-tui-lib` + `json-tui` and never resolves,
fetches, or builds googletest — the upstream `JSON_TUI_BUILD_TESTS=OFF`
default, recovered. `cpp-pkg test` builds gtest on first use and runs
`tests` (default invocation; gtest self-reports). Note: the `-Werror` ×
gtest-header break (GAPS §4/§4b) is *narrowed* by this design (app-only
consumers can't hit it) but only *fixed* by B1's `-isystem` probe fix +
per-target flags — see §6.1.

### 5.2 vtz (the runner-surface stress test: 774 cases, README lines 88–98)

Before — manual protocol hand-copied into README:

```sh
cd tzdb-runtime
../build/test_vtz     --build . --testdata ../etc/testdata
../build/test_vtz_api --build . --testdata ../etc/testdata
../build/test_vtz_api --no_set_install --build . --testdata ../etc/testdata
../build/test_tzdb_load "$PWD/data/tzdata"                       # set_install path
VTZ_TZDATA_PATH="$PWD/data/tzdata" ../build/test_tzdb_load       # env path
VTZ_TZDATA_PATH=/bad/env/path ../build/test_tzdb_load; test $? -ne 0  # death test
```

and manifest comments "Test-only deps (no way to mark them test-only in
v0)…" (CppPkg.toml:58–109) with GTest/date/benchmark/absl as regular deps.

After — deps: `fmt`, `unordered_dense` stay in `[dependencies]` (shipped
targets use them); `GTest`, `date` (with its `exposes-targets =
["Threads::Threads"]` line), `benchmark`, `absl` move verbatim to
`[dev-dependencies.*]`. Targets:

```toml
[targets.vtz_testing]
type = "static-library"
dev  = true                                    # upstream: test-support lib
# ...sources/includes/deps unchanged (GTest::gtest, date::date-tz, ...)

[targets.test_vtz]
type = "executable"
test = true
sources = ["etc/test/test_impl/*.cpp"]
cxx-std = 17
dependencies = ["vtz_testing", "vtz_extras"]

[[targets.test_vtz.run]]
cwd  = "tzdb-runtime"
args = ["--build", ".", "--testdata", "../etc/testdata"]

[targets.test_vtz_api]
type = "executable"
test = true
sources = ["etc/test/test_api/*.cpp"]
cxx-std = 17
dependencies = ["vtz_testing", "vtz"]

[[targets.test_vtz_api.run]]
name = "installed-tzdb"
cwd  = "tzdb-runtime"
args = ["--build", ".", "--testdata", "../etc/testdata"]

[[targets.test_vtz_api.run]]
name = "embedded-tzdb"
cwd  = "tzdb-runtime"
args = ["--no_set_install", "--build", ".", "--testdata", "../etc/testdata"]

[targets.test_tzdb_load]
type = "executable"
test = true
sources = ["etc/test/standalone/test_tzdb_load.cpp"]
cxx-std = 17
dependencies = ["vtz"]

[[targets.test_tzdb_load.run]]
name       = "set-install-path"
cwd        = "tzdb-runtime"
args       = ["${project-root}/tzdb-runtime/data/tzdata"]
env-remove = ["VTZ_TZDATA_PATH"]               # upstream's unset-var matrix

[[targets.test_tzdb_load.run]]
name = "env-path"
cwd  = "tzdb-runtime"
env  = { VTZ_TZDATA_PATH = "${project-root}/tzdb-runtime/data/tzdata" }

[[targets.test_tzdb_load.run]]
name = "death-bad-env-path"
cwd  = "tzdb-runtime"
env  = { VTZ_TZDATA_PATH = "/bad/env/path" }
expect-failure = true

[targets.bench_vtz]
type = "executable"
dev  = true                                    # dev graph, not a test
# ...unchanged (benchmark::benchmark, absl::time, date::date-tz now legal
# because bench_vtz is dev)
```

`cpp-pkg test` reproduces the entire README protocol: 6 invocations,
774 self-reported cases, death tests included. What this design does
**not** absorb: provisioning `tzdb-runtime/` itself (the tzdb
fetch/extract/zic flow) — that remains `scripts/fetch-tzdata.sh`, routed
to B4's deferred `[assets]` tier (§6.3). The runner *requires* the fixture
cwd to exist and fails that invocation with a clear message if the script
wasn't run — strictly better than today's silent reliance on README order.

`cpp-pkg build` now builds: `vtz`, `vtz_impl`, `vtz_extras`, 4 examples,
`dump_tzfile` — and no longer fetches GTest/date/benchmark/absl, i.e. a
consumer of vtz-the-library stops paying for four dep builds (two of which
currently need locally-patched clones!).

### 5.3 ninja

Before: `[dependencies.googletest]` unconditional (GAPS §3: "a famously
zero-dep project now declares one"); `ninja_test` an ordinary executable;
runner = README prose ("run from a writable scratch dir").

After: googletest → `[dev-dependencies]`; `[dependencies]` is **empty
again** — the manifest's declarative reading matches ninja's identity.

```toml
[targets.ninja_test]
type = "executable"
test = true
# sources/deps unchanged (libninja, GTest::gtest)

[[targets.ninja_test.run]]
cwd = "build/test-scratch"        # inside build dir → runner creates it
```

### 5.4 googletest (native port, mode a)

Before: 10 sample tests as plain executables; runner =
`for i in $(seq 1 10); build/sample${i}_unittest; done` with the user
aggregating exit codes (GAPS §1).

After: `test = true` on each of the 10 sample targets (one added line
each, no run tables needed); `cpp-pkg test` = build + run + aggregate,
`cpp-pkg test sample5_unittest -- --gtest_filter=...` for one case. Gap 3
(samples always built for every consumer) is also closed for the test
samples: they leave the default build.

### 5.5 benchmark

Before: suite dropped entirely (GAPS §5 — gtest would poison the closure;
~40 CTest registrations inexpressible; only `basic_test.cc` salvaged as a
plain executable).

After: googletest → `[dev-dependencies]` (their
`BENCHMARK_USE_BUNDLED_GTEST` bundled-download machinery is simply the
store); per-test targets marked `test = true` with runs like
`args = ["--benchmark_min_time=0.01"]`. Honest residue: the
**output-checked** tests (compare stdout against expectations) have no
harness here — out of scope by charter, recorded; and the suite's
`-UNDEBUG` / per-test flags need B1 to land (§6.1) before those specific
targets compile with upstream fidelity.

### 5.6 abseil

Before: "full port declared impossible until this exists" (GAPS
testing-story): 241 `absl_cc_test` exes + 40 TESTONLY libs unportable.

After: the `gen_toml.py` generator emits `dev = true` for `TESTONLY = 1`
libraries (an exact 1:1 mapping) and `test = true` (+ nothing else — absl
tests need no args/cwd) for `absl_cc_test` targets; gtest becomes one
`[dev-dependencies]` entry. The remaining blockers for the full port are
B9 (repetition) and B5 (patches), not testing.

### 5.7 cppcheck / cpptrace

cppcheck: `testrunner` = `test = true` + a run entry (args style
`--tinyxml2`); but it also needs "cli library minus main.cpp" (B8 glob
exclusion) and cfg/ runtime-data staged next to the binary (B6
runtime-data) before the suite actually passes — testing provides the
socket, those provide the plug. cpptrace: gtest/FetchContent →
`[dev-dependencies]`, unit tests marked; the per-test `-g`/split-dwarf
knobs are B1 per-target flags and compose with no interaction (flags on a
test target are just flags on a target).

---

## 6. Interaction analysis with the other areas

### 6.1 × Per-target flags (B1/S1)

Test targets are ordinary targets; whatever flag surface wins
(S1-A/B/C), it applies to them with zero special cases. The json-tui break
decomposes cleanly across the two designs: testing removes gtest from the
*app consumer's* world entirely; B1's `-isystem` probe fix + moving
`-Werror` from profile scope to the lib/app targets makes `cpp-pkg test`
itself clean (upstream shields `tests` from the warning set — expressible
the moment flags are per-target). Neither design depends on the other;
each is honest alone, together they reproduce upstream exactly. One
deliberate non-interaction: run entries carry **no flag keys** — build
configuration stays in the target, run configuration in `run`.

### 6.2 × Platform conditionals (B3/S3)

- `dev` / `test` markers are **not cfg-conditional** in v1: a target's
  dev-graph membership is platform-independent in all wave-1 evidence, and
  a cfg-varying test set would make `cpp-pkg test`'s meaning
  platform-dependent in a way the declarative reading hides.
- Run-entry *contents* will eventually want cfg (a Windows cwd, a
  linux-only death test). Decision now: S3 sub-tables (if S3-A wins) merge
  into scalar/list fields of targets, but **do not reach inside `run`
  arrays in v1** — arrays-of-tables have no obvious merge semantics
  (append? index-match?), and guessing wrong is worse than waiting for
  Linux bring-up to produce a concrete case. Flagged to S3's designer.
- Dev-deps accept whatever cfg form deps get (`[dev-dependencies.x.cfg.linux]`
  works by construction — same `DependencySpec`).

### 6.3 × Codegen (B4/S4)

- **Shared grammar:** `${project-root}`/`${build-dir}` in run entries use
  the same `${...}` interpolation syntax and resolver as S4's `vars` /
  `${gen}` / `${pin.*}` — one module (say `interp.rs`), namespaces kept
  disjoint, both designs must cite it (S4's designer pinged in its Open
  Questions too). Two interpolation grammars would be an unforced error.
- **Fixture provisioning:** vtz's tzdb runtime data is S4's deferred
  `[assets]` tier (d) + B6's `runtime-data`; the runner's contract is
  merely "cwd exists or the invocation fails legibly". When `[assets]`
  lands, a future `[[run]] needs-assets = ["tzdb"]` is additive. Do not
  block testing on it — vtz's script workaround is explicitly rated
  "good" in its GAPS.
- **Generated test sources:** if a test target's sources include
  `${gen}` outputs, ordinary build-graph ordering (generate edges before
  compile edges) covers it; the runner sits strictly after the build and
  never sees codegen.

### 6.4 × Install/export (B6) and defaults (B9)

- Export emitters (manifest + Config shim from local `[targets]`) skip
  `dev`/`test` targets and never emit `find_dependency` for dev-deps —
  this single rule is what makes `[dev-dependencies]` "out of the export
  graph" rather than just out of the default build.
- `[target-defaults]` (B9): the `dev`/`test` markers are **excluded** from
  defaultable keys (a default that silently reclassifies every target's
  graph membership fails "the declarative reading must never lie");
  `run` arrays likewise. cxx-std/flags/defines defaults apply to test
  targets like any target.
- B7 pseudo-packages: Threads dedup applies across both dep tables (vtz's
  four Threads importers are all dev-deps after migration).

---

## 7. Linux story (explicit — next campaign stage)

- **Runner:** `std::process::Command` throughout — argv-array spawn, no
  shell, so zsh/bash/dash differences cannot exist; cwd/env semantics are
  identical on macOS and Arch. Exit classification uses
  `ExitStatus::code()`/`ExitStatusExt::signal()` (unix): signal death is
  "failed" (or "passed" under `expect-failure`, per §1.2). This matters
  concretely on Linux: glibc assertion aborts and `_FORTIFY_SOURCE` traps
  make SIGABRT deaths *more* common there, and vtz's death tests must pass
  under gcc 16 the same way they do under Apple clang.
- **Schema:** nothing in this design is platform-conditional — no macOS
  projection is added. `dev`/`test`/`run` mean the same thing on every OS;
  the platform-varying parts of a test (which sources, which defines) are
  B3's job on the *target*, not this design's.
- **Dev-deps:** ride the same fetch/cmake_build/probe/store pipeline whose
  Linux bring-up is already the campaign's next stage; being lazy, they
  add zero new bring-up surface (a Linux `cpp-pkg build` of json-tui never
  touches googletest at all — dev-deps are actually the *easiest* part of
  the Linux story because they can be deferred per-project).
- **Deferred:** Windows (`.exe` suffix, signal-less exit codes,
  `env` case-insensitivity) is out of v1 scope, consistent with the rest
  of the tool; noted so the `expect-failure` wording ("exited nonzero or
  died by signal") is revisited once Windows exists.

---

## 8. Implementation sketch (src/ modules)

- **schema.rs** (~80 lines): `TargetSpec` += `dev: bool`, `test: bool`
  (serde defaults `false`; T1: `TargetKind::Test` instead of the bool),
  `run: Vec<RunSpec>`; new `RunSpec { name, args, cwd, env, env_remove,
  expect_failure }` with `deny_unknown_fields`; `ProjectFile` +=
  `dev_dependencies: BTreeMap<String, DependencySpec>`; load-time
  validation (test-on-library, run-on-non-test, key collision,
  cross-table `needs` direction, duplicate run names).
- **cli.rs** (~150 lines): extract steps 1–6 of the `Build` arm into a
  shared `fn provision_and_plan(requested: &TargetSet, ...)`; the dep
  loop in step 4 iterates `[dependencies]` always and `[dev-dependencies]`
  only when the requested set contains dev targets (restricted to the
  reachable dev-dep subset); new `Cmd::Test { filters, config, toolchain,
  jobs, list, verbose, passthrough }` = provision_and_plan(selected tests)
  → ninja → `runner::run`.
- **graph.rs** (~100 lines): `plan(only: &[])` currently means "all
  targets" — becomes "all non-dev targets"; explicit names keep working
  for dev targets. `build_exposed_table` ingests dev-dep manifests when
  present; a post-resolution visibility pass walks edges and emits the
  §1.1 error (it has the owner table, so the message can name the owning
  dev-dep).
- **runner.rs** (new, ~250 lines): expand run entries (default invocation
  when empty), interpolate via the shared resolver, create-if-under-build
  cwd rule, spawn serial or `--jobs`, capture/replay output, aggregate,
  `--list`.
- **interp.rs** (new, shared with S4, ~60 lines): `${...}` splitter +
  namespace dispatch; testing registers `project-root`/`build-dir`.
- **Unchanged:** hashing.rs, store.rs, fetch.rs, cmake_build.rs, probe.rs,
  lockfile.rs (dev-deps reuse `[[package]]` verbatim), ninja_gen.rs
  (driven by the plan; build.ninja simply contains the requested closure —
  it is already regenerated every build). shim.rs gains the dev-exclusion
  filter when B6 lands, not now.
- **Query note:** `--query`/compile_commands.json cover whatever was last
  planned; test TUs appear after the first `cpp-pkg test` (their dev-dep
  manifests must exist in the store to plan compile edges). Documented
  behavior, not a bug.
- Total ≈ 650–800 LOC + integration-test fixture project (pass / fail /
  death / env-matrix / multi-run cases).

---

## 9. Honest costs (all candidates)

- **A build tool grows a process supervisor.** The runner's scope will be
  pushed on immediately: timeouts, retries, JUnit/XML output, per-case
  gtest parsing, output matching (benchmark), coverage. The no-discovery
  decision is the bulwark; each addition must re-justify itself against
  "gtest binaries already self-report". Expect to say no a lot.
- **Aggregation granularity:** vtz's 774 cases report as 6 lines; a single
  failing case fails a whole invocation, and localization is
  `cpp-pkg test X -- --gtest_filter=...` by hand. This is exactly CTest's
  granularity for non-discovered tests — parity with upstream's *worse*
  mode, not its best mode.
- **Two dependency tables** = a new resolution rule (visibility) users
  must learn; the error message carries the whole teaching burden.
- **Default-build change** is an observable behavior change the moment a
  manifest adopts markers (intended, but it means "cpp-pkg build builds
  everything" stops being true as a slogan).
- **Lazy dev-dep locking** means CppPkg.lock is not the complete universe
  until the first `cpp-pkg test` on some machine — a CI that only ever
  builds will not pin gtest. (Open question below.)
- **Not covered, by design:** fixture *provisioning* (vtz tzdb → B4
  assets), runtime-data staging (cppcheck → B6), output-checked tests
  (benchmark), per-case discovery, Windows. Each is named where it's
  routed; none silently pretend to be covered.

---

## OPEN QUESTIONS for the taste judge

1. **Marking spelling — the A/B/C call (T1 vs T2 vs T3).** The corpus
   (R1: TESTONLY libraries; benchmark's dual-role binaries) pressures
   toward T2's orthogonal `dev`+`test`; T1 is one line shorter in the
   common case; T3 is the only v1 that smoke-tests shipped binaries.
   Designer recommends **T2** (reasons in final summary). Pick and log the
   runner-up.
2. **The `dev` name.** `dev = true` (Cargo family) vs `test-only = true`
   (Bazel family, but a lie for benches) vs splitting (`test-only` +
   something for benches — rejected here as two special cases). Also: is
   `test = true` on an executable the right spelling, or should T2 adopt
   T1's `type = "test"` purely as *sugar* for `executable + test` (one
   more spelling of the same fact — charter (4) tension)?
3. **Dual-role reservation:** should `test = true, dev = false` (ships AND
   runs under `cpp-pkg test`) be legal in v1, or stay reserved/error?
   Benchmark's suite is the only near-evidence; T3 gets it for free.
4. **Lockfile eagerness:** should plain `cpp-pkg build` resolve+lock
   dev-deps (network cost for consumers who'll never test — violates the
   strong reading of "consumers don't pay") or stay fully lazy (CppPkg.lock
   incomplete until first test — surprises Cargo users)? Designer leans
   lazy; either is a one-line change in the dep loop.
5. **`expect-failure` vs signals:** is "nonzero exit OR signal death"
   (shell semantics, matches vtz's actual check) acceptable, or should
   signal deaths always fail (CTest `WILL_FAIL` semantics) with an
   explicit `expect-signal` later if needed?
6. **Runner defaults:** serial-by-default with `--jobs` opt-in (fixture
   safety, CTest precedent) vs parallel-by-default (Cargo precedent,
   faster, races vtz's shared fixture cwd)?
7. **Interpolation ownership:** confirm the single shared `${...}` grammar
   with S4 (namespaces: `package.*`/`pin.*`/`gen` for codegen,
   `project-root`/`build-dir` for runs), and whether `${cwd}` earns a slot
   (vtz's `$PWD` args are spellable without it, via `${project-root}`).
8. **`cpp-pkg build --tests` / `--all`:** worth a flag to build (not run)
   all dev targets — e.g. warming CI caches — or is explicit naming
   enough for v1?
