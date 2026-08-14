# Wave-1 Schema Extensions — NORMATIVE

Status: **NORMATIVE for S3 implementation** (S2 design round, taste
judgment 2026-08-14; red-team pass adjudicated and folded in 2026-08-14 —
16 findings, 15 spec fixes, 1 behavior upheld as an explicit logged
decision: the §5.4 `exposes-targets` flag-day error). This is the
single implementation-wave reference: final TOML surfaces, semantics, hashing
impact, error messages, Linux behavior, and per-project migration notes.
Where this file disagrees with a candidates file under
`docs/design/candidates/`, this file wins. Where it is silent, the winning
candidate's text is authoritative (winners listed in §0.7).

Authority: CAMPAIGN.md taste charter (binding). Evidence: migrations/BACKLOG.md
+ per-project GAPS.md. Baseline schema: CPPKG_TOML.md (v0) — this document is
the delta; CPPKG_TOML.md must be updated to fold it in when the wave lands.

Contested aesthetic calls and expensive-to-reverse decisions are additionally
logged in the Taste Memo / MORNING_REPORT.md (per the autonomy protocol).

---

## 0. The language as one language (cross-cutting rulings)

These rulings exist because six areas designed in parallel must read as one
schema. Several overrule details of individual candidate docs; each overrule
is marked **[coherence ruling]**.

### 0.1 One value grammar: the visibility split

`includes`, `defines`, `dependencies`, and (new) `cxx-flags` / `c-flags` /
`link-flags` on targets all share the exact same shape:

```toml
key = ["a", "b"]                          # bare list == all-private (sugar)
key = { public = [...], private = [...] } # full form
```

No new value grammars are introduced anywhere in this wave. cfg blocks
(§2), target-defaults (§7.2), and export emission (§6) all operate on this
one shape. Testing's `[[run]]` entries and `[generate.*]` steps are tables of
scalars/lists, not new list-entry micro-grammars.

### 0.2 One cfg placement rule **[coherence ruling]**

The candidate docs disagreed on where conditional flags live
(`[cfg.linux.flags]` vs `[flags.cfg.linux]`). Normative rule:

> **A `cfg.<predicate>` sub-table nests inside the scope it conditions,
> wherever that scope's key-space permits. Collections keyed by user-chosen
> names cannot host a magic `cfg` key, so they are conditioned at package
> scope under `[cfg.<predicate>]`.**

Concretely, in v1:

| Conditioned scope | Spelling |
|---|---|
| a target | `[targets.<t>.cfg.<pred>]` |
| the package flags layer | `[flags.cfg.<pred>]` |
| dependency presence | `[cfg.<pred>.dependencies.<key>]` |
| dev-dependency presence | `[cfg.<pred>.dev-dependencies.<key>]` |

Reserved (distinct "reserved, not in v1" error): `[target-defaults.cfg.<pred>]`,
`[cfg.<pred>.targets.<name>]` (whole conditional targets),
`[cfg.<pred>.generate.<name>]`, `[profiles.<p>.cfg.<pred>]`.

Inline `when = "..."` entries are **rejected, not reserved** — one spelling
of a conditional per language; two is how dialects fork. (Runner-up logged in
the Taste Memo.)

### 0.3 One interpolation grammar: `${...}` (closed vocabulary, whitelisted positions)

A single grammar and a single resolver (`src/interp.rs`) serve codegen,
testing, and install. The vocabulary is **closed**; an unknown variable is a
hard error naming the vocabulary (never empty-substitution). `$${` escapes a
literal `${`. Outside the whitelisted positions, `${` is a hard error — the
grammar cannot creep.

| Variable | Value | Positions |
|---|---|---|
| `${package.name}` | `[package].name` | defines values; `[generate.*]` vars/argv |
| `${package.version}` (+ `.major/.minor/.patch`) | `[package].version` (error if unset; component error if non-integer) | same |
| `${pin.<depkey>.commit}` | resolved commit sha from CppPkg.lock (base commit — never the patch-composed id, §5.2) | defines values; `[generate.*]` vars/argv |
| `${pin.<depkey>.requested}` | human ref (`v1.9.5` for `tag:`, sha for `rev:`) | same |
| `${gen}` | the generated-output root `build/gen/` | `sources`/`includes` entries; `[generate.*]` argv, `inputs` entries, and `stdin` (a `${gen}` input creates the producer edge implicitly, §4.1); run-entry `args`/`cwd`/`env` values |
| `${project-root}`, `${build-dir}` | absolute paths | run-entry `args`/`cwd`/`env` values |
| `${install-prefix}` | `install --prefix` value; default `/usr/local` (overridable via `build --prefix`) | defines values only |
| `${pin.self.*}` | **reserved, not implemented.** When implemented: resolves from the consumer's lockfile in dependency mode; in a root build it is a **hard error** ("only meaningful when built as a dependency; use `${package.version}`") | — |

**[coherence ruling]** Run entries accept `${gen}` (the testing doc listed
only two variables; the codegen doc's fixture hook — vtz's
`env = { VTZ_TZDATA_PATH = "${gen}/zoneinfo" }` — is the corpus case, so
`${gen}` is in). `${cwd}` is not a variable.

### 0.4 One exclusion grammar: `!`-prefixed negative patterns

`sources`, `runtime-data.patterns`, and `public-headers.patterns` all accept
`!`-prefixed globs. Semantics (order-independent): union of positives minus
union of negatives, applied after expansion, before the existing
lexicographic sort. A negative matching nothing → **warning** (upstream may
have deleted the file; surface drift, don't break). Positives expanding to
nothing → existing hard error. A list of only negatives → schema error.

### 0.5 Merge and layering conventions (stated once)

- **cfg merge is additive-append only** (§2.2): conditional entries append
  after unconditional entries of the same key; scalars under cfg are a hard
  error in v1.
- **[target-defaults] merge** **[coherence ruling]**: scalar keys —
  fill-if-absent (target value wins); list/visibility keys — default entries
  **prepend**, target entries append. Rationale: "every target gets these"
  must stay true when a target adds its own entries (tie-breaker 1); the
  flags doc's fill-if-absent assumption is overruled. Combined order for a
  list key: defaults-unconditional → defaults-cfg (reserved) →
  target-unconditional → target-cfg.
- **Flag ordering is last-wins, documented contract** (promoted from
  accident; closes the B12 item). Full compile-line layering in §1.3.

### 0.6 Resolution-ladder step 0: builtin pseudo-packages

The naming ladder (CPPKG_TOML.md) gains a step 0: builtin names (`v1` list:
`Threads::Threads` only) resolve first and cannot be shadowed.
`exposes-namespace`/`exposes-targets` claiming a builtin is a hard error with
the fix in the message ("builtin pseudo-package; delete this line").

### 0.7 Decision table

| Area | Winner | Runner-up (Taste Memo) | Character |
|---|---|---|---|
| B1 flags | Candidate **C** — visibility-split grammar + propagation-class fence | A (open strings) | contested; judged |
| B3 cfg | Candidate **A** — cfg sub-tables | B (inline `when`) | contested; judged; **near-irreversible** |
| B2 testing | **T2** — orthogonal `dev` + `test` markers + `[[run]]` | T1 (`type = "test"`) | contested; judged |
| B4 codegen | Candidate **1** — `[generate.<name>]`, two tiers, `checked-in` mode included | 2 (target-attached templates) | mostly engineering |
| B5+B7 patches/sysdeps | Candidate **A** — one table, `system = true`; `pkg-config` field **reserved** | B (two tables, pkg-config-first) | contested; judged |
| B6 install/export | Candidate **B** — derived headers, `install = true` | A (declared `public-headers` required) | contested; judged |
| B8 glob exclusion | `!` negation in existing arrays | — | ratified as specced |
| B9 target-defaults | `[target-defaults]` with §0.5 merge | — | ratified with merge fix |

---

## 1. Per-target flags and flag layering (B1) — decided: S1-C

### 1.1 Surface

```toml
[flags]                          # NEW top-level: all targets, all profiles
cxx-flags  = ["-Wno-deprecated"]
c-flags    = []
link-flags = []

[flags.cfg.linux]                # conditional refinement (§0.2)
cxx-flags = [...]

[targets.json-tui-lib]
cxx-flags = { private = ["-Wall", "-Wextra", "-pedantic", "-Werror",
                         "-Wmissing-declarations", "-Wshadow"],
              public  = ["-fno-exceptions"] }
link-flags = ["-Wl,-framework,CoreFoundation"]   # bare list == all-private

[targets.tinyxml2]               # vendored-code parity (cppcheck)
system-includes = true           # consumers get this target's public includes
                                 # as -isystem; its own TUs still see -I

[dependencies.ftxui]
system-includes = false          # opt out of -isystem for this dep's headers
```

- `cxx-flags` / `c-flags` / `link-flags` on any target; value per §0.1.
  Language routing as in profiles (`cxx-flags` → C++ driver only).
- Flags are **argv words, not shell text**; two-word flags are two entries.
- `[flags]` is hoisted-profile semantics: consumer-targets-only, except
  ABI-classified entries, which inject into dependency builds and fold into
  each dependency's config hash via the **existing** profile-ABI machinery.
  No visibility split at `[flags]` scope (it is environment, not interface).
- `system-includes`: default `true` on dependencies, `false` on project
  targets. Distinct from `system = true` (a *system dependency*, §5.3) —
  document both side by side.

### 1.2 The propagation fence (what makes this C, not A)

Public-bucket entries are checked at manifest load against the classifier in
`toolchain.rs` (the existing ABI table, extended). Private is fully open.
**Unknown flags fail open (allowed)** — the fence only rejects four classes
whose propagation is categorically wrong:

| class | v1 members | public bucket |
|---|---|---|
| ABI | existing table (`-D_GLIBCXX_*`, `-stdlib=*`, `-f*abi*`, …) | error (and error at **any** target scope, §1.4) |
| sanitizer | `-fsanitize*` | error |
| warning | `-W…` except `-Wl,`/`-Wa,`/`-Wp,` pass-throughs; `-w` | error: "warnings are private by nature; a library cannot volunteer its consumers into a diagnostic policy" |
| opt/debug | `-O*`, `-g`, `-g[0-9]`, `-ggdb*`, `-glldb*` | error: "optimization level is the consumer's (profile's) decision" |
| everything else | `-f…`, `-m…`, `-pthread`, unknown | allowed |

**Classification unwraps driver pass-through spellings before matching**:
`-Wl,`/`-Wa,`/`-Wp,` prefixes are stripped and their comma-separated
payload words classified individually — `-Wp,-D_GLIBCXX_DEBUG` is caught
by the ABI table (at any scope, per §1.4), while `-Wl,-framework,X` still
passes (`-framework` is in no rejected class). The two-argv forms
`-Xlinker`/`-Xpreprocessor`/`-Xassembler <word>` classify the following
word the same way. The warning-class exemption for pass-throughs stands —
they are transport, not warnings — but transport never launders ABI or
sanitizer payloads through the fence.

`link-flags` public bucket: only ABI/sanitizer classes checked; `-Wl,…`
passes (after unwrapping, as above). Public flags on an `executable`:
validation error ("nothing can consume an executable").

Knob sugar (`exceptions = false`, `rtti = false`) is **deferred** — one
corpus site; purely additive later as checked sugar over the open surface.

### 1.3 Layering (normative, last-wins)

Compile line for a TU of target `T`, left to right:

1. Toolchain/driver defaults + built-in config flags of the selected profile.
2. ABI injection set (ABI-classified entries of `[flags]` + selected profile).
3. `[flags]` (non-ABI remainder), then its matching `cfg` groups.
4. `[profiles.<selected>]` flags.
5. Public flags propagated from `T`'s transitive compile-visible
   dependencies (topological order, **deduplicated by contributing target**,
   never by flag string; diamonds contribute once).
6. `T`'s own public flags (unconditional entries, then matching cfg
   groups' public entries — §0.5: a cfg entry joins its own key *and
   visibility bucket*, it does not trail the whole target).
7. `T`'s own private flags (unconditional, then matching cfg groups'
   private entries) — most specific voice last.

Step 5 collects the *same* §0.5-merged public lists that step 6 emits
locally — propagation and local emission never read cfg placement
differently, so a `[targets.x.cfg.linux] cxx-flags = { public = [...] }`
entry lands in one and only one position everywhere it appears.

Link line: `T`'s objects → `[flags].link-flags` → profile `link-flags` →
`T`'s own `link-flags` → the link closure **interleaved**: each closure
member's library input immediately followed by that member's own
`link-flags` contribution (existing deterministic
dependents-before-dependencies order, each contributor once).
**[coherence ruling]** Closure-collected link-flags are *not* emitted as a
trailing block: a contributor's raw `-lrt`-class words must land after the
archive whose objects reference them, or GNU ld under `--as-needed`
(Arch's default) discards the library before any undefined reference has
been seen (abseil `base` → `-lrt` → `shm_open` is the corpus case).
`T`'s own link-flags follow `T`'s objects for the same reason.

`link-flags` on a `static-library` propagate link-only to consumers' final
links (same rule as private deps / `$<LINK_ONLY>`). Consequence, documented:
public≡private for `link-flags` on static libraries until a shared-library
kind exists (accepted; rejecting the public bucket there would be a
gratuitous asymmetry with `cxx-flags`).

### 1.4 Errors and lints

- ABI-classified flag at target scope → hard error:
  `"-stdlib=libc++" affects the ABI of the entire link closure including
  store dependencies; move it to [flags] or a [profiles.*] block, where it
  will propagate to dependency builds and their config hashes.`
- Fence rejection (§1.2) → load-time hard error, one line, naming the class.
- `-D*`/`-U*`/`-I*`/`-isystem`/`-std=*` inside flag lists → **warning**
  naming the dedicated key (`defines`/`includes`/`cxx-std`). Not an error:
  migrations paste flag soup and `-UNDEBUG` has no schema home (benchmark's
  tests legitimately need it).
- `-fsanitize=*` anywhere below profile scope: consumer-only + the existing
  "dependencies are uninstrumented" warning.

### 1.5 Hashing

- Target-level flags and non-ABI `[flags]` entries: **no** dependency
  config-hash contribution (asserted by test).
- ABI-classified `[flags]` entries: fold into dependency config hashes via
  the existing profile-ABI rule. Empty layers hash to nothing — no existing
  store entry is invalidated.

### 1.6 Linux behavior

Mechanism is platform-neutral (opaque argv words). `-isystem` behaves
identically on GNU drivers (gcc 16 / clang 22 both honor suppression +
search-order). Propagated link-flags interleave after their contributing
archives (§1.3) — the only emission position where raw `-l` words survive
Arch's default `--as-needed`; dependents-first *block* emission is not
sufficient and is not what ships. Compiler-conditional flags
(`-Wshorten-64-to-32`) go under `cfg.clang` (§2) and become load-bearing on
keres. `-fPIC` via `[flags]` reaches project targets uniformly and is not
ABI-classified (store deps make their own PIC choice). MSVC flag spellings:
future classifier additions behind `cfg.msvc`.

### 1.7 Migration notes (dissolved workarounds)

- **json-tui**: both duplicated profile stanzas deleted; the wave-1 build
  break dissolves with **zero suppressions** (`-Werror` moves to
  target-private; gtest headers arrive `-isystem` via the tool fix;
  `-Wno-error=character-conversion` deleted). `-fno-exceptions` public on
  the lib = upstream's structure, line for line.
- **ninja**: 4× identical `[profiles.*]` stanzas → one `[flags]` line.
- **abseil**: COPTS restored as `[flags.cfg.clang]` + `[flags.cfg.gcc]`
  blocks mirroring upstream's own per-compiler split
  (`ABSL_LLVM_FLAGS` / `ABSL_GCC_FLAGS` — one unconditional block would
  spray unknown-warning errors under whichever driver it wasn't written
  for, failing §2.4's two-compiler gate); ~15 lines each in header.toml,
  zero per-target lines emitted by the generator;
  `-Wl,-framework,CoreFoundation` moves from profile scope (where `--config
  debug` silently lost it) to `[targets.time.cfg.macos] link-flags`.
- **cppcheck**: full `-Weverything`-with-vendored-exemptions policy lands
  under `[flags.cfg.clang]` (`-Weverything` and its curated `-Wno-*` list
  are clang vocabulary; gcc builds get the portable subset in `[flags]`) +
  `system-includes = true` + per-target `-Wno-*` on tinyxml2/simplecpp;
  `[profiles.release] cxx-flags = ["-O2"]` stays put —
  profile-over-builtin last-wins is now contract.
- **benchmark**: warning battery splits: portable members
  (`-Wall -Wextra -Wshadow -pedantic -pedantic-errors -Werror` …) in
  `[flags]`, clang-only members (`-Wthread-safety`) under
  `[flags.cfg.clang]` — with `-Werror` in the same battery, one
  unconditional block is a hard build break on gcc 16 (§2.4's gate);
  test-only suppressions scoped to their targets; `donotoptimize_test`'s
  `-O3`/`-UNDEBUG` override works by the documented layering.
- **vtz / cpptrace / googletest**: profile-hoisted sets move to `[flags]` or
  target-private per upstream's actual scoping (details in candidates
  flags.md §4.6).

---

## 2. Platform conditionals (B3) — decided: cfg sub-tables (S3-A)

> **Near-irreversible** (flagged in the morning report): this grammar will
> exist in user files at every list key. Everything in candidates/cfg.md §0
> (shared semantics) is adopted verbatim; the surface is Candidate A with
> the §0.2 placement rule.

### 2.1 Predicate vocabulary (closed, v1)

| Axis | Atoms | Truth source |
|---|---|---|
| os | `windows`, `macos`, `linux` | parsed from `ToolchainIdentity.target_triple` |
| os family | `unix` | true iff os ∈ {macos, linux}; future unixes join additively |
| compiler | `clang`, `gcc`, `msvc` | `compiler_id` mapping; **`clang` matches AppleClang** (googletest STREQUAL footgun) |

- `unix` is **kept** (ninja's canonical split reads with it; a future BSD
  joining the family is the intended semantics).
- `apple-clang` is reserved, not shipped (no wave-1 need).
- Unknown atom → hard error at load listing the vocabulary. Combinators
  (`all(...)`, `any(...)`, `not(...)`, version comparisons) → distinct
  "reserved, not available in v1" error. The **blessed reserved combinator
  spelling** is the quoted key: `[targets.x.cfg."all(linux, gcc)"]`. Probe
  predicates (`"has-symbol(ppoll)"`) will slot into the same quoted-key
  positions — layer 2, out of scope.

### 2.2 Semantics (adopted from candidates/cfg.md §0, summarized)

- Evaluated at plan time from toolchain identity (target answers "what am I
  building *for*"), before glob expansion; every matching group applies.
- **Additive-append merge** onto the same key; unconditional first, then
  matching groups in document order. Conditionable keys: `sources`,
  `includes`, `defines`, `dependencies` (target scope), `cxx-flags`,
  `c-flags`, `link-flags`, and the B6 list key `runtime-data` —
  list-valued keys only. **`public-headers` is not conditionable**: §6.4
  defines it as a total override, never merged, and cfg's only merge is
  additive-append — the two semantics cannot compose, and a cfg-only
  override would silently flip a target between derived and declared
  headers per platform. Hard error whose message is the fix: condition
  `includes.public` instead — header derivation follows the cfg
  projection for free (§6.4). Scalars in a cfg group → hard error
  "conditional scalar overrides are not in v1" (door not welded shut; the
  error says v1).
- Non-matching groups are validated (vocabulary, key rules) but never
  expanded: their globs unexpanded, paths unchecked.
- **All declared deps are resolved and locked, always** (windows-only deps
  lock from a Mac); only active-predicate deps are fetched/built/probed.
  Reference to a cfg'd-out dep's target from an active target → unresolved
  reference error augmented with `declared behind cfg '<pred>', which is
  false for this toolchain`. `needs` reaching a cfg'd-out dep → resolve
  error naming the predicate.
- A dep key declared in two places (unconditional + conditional, or two
  branches) → hard error in v1.
- Nested cfg inside cfg → hard error. Empty group → lint warning
  (generators emit them). Zero sources after evaluation → existing error,
  extended to name the non-matching groups.
- **No config-hash impact**: cfg is a project-manifest concern; dep-side
  platform variation is per-toolchain store re-extraction (gated on the B12
  `$<BOOL>` LINK_ONLY fix, Appendix A).

### 2.3 Canonical examples (normative shapes)

```toml
[targets.libninja]
sources = ["src/build.cc", ...]              # 27 platform-neutral files

[targets.libninja.cfg.unix]
sources = ["src/jobserver-posix.cc", "src/subprocess-posix.cc"]

[targets.libninja.cfg.linux]
# transcribed: check_cxx_symbol_exists(ppoll ...) — layer 2 owns the probe
defines = { private = ["USE_PPOLL"] }

[targets.libninja.cfg.windows]
sources = ["src/subprocess-win32.cc", "src/getopt.c", ...]
defines = { private = ["NOMINMAX"] }

[targets.time.cfg.macos]
link-flags = ["-Wl,-framework,CoreFoundation"]

[targets.vtz.cfg.clang]
cxx-flags = ["-Wshorten-64-to-32"]

[cfg.windows.dependencies.winreg]
git = "..."
tag = "..."
```

**Labeled transcription is a normative documentation convention**: a
transcribed probe answer lives under the narrowest true predicate with a
comment `# transcribed: <upstream check>` naming the check (and the
knowingly-wrong axis where applicable — cppcheck's musl caveat). This is the
blessed layer-2 interim; the comment prefix is stable so it is lintable and
mechanically migratable when probes land.

### 2.4 Linux behavior

cfg **is** the Linux enabler; acceptance test: one committed manifest,
macOS + Arch (gcc 16 / clang 22), both green, unedited. Bring-up
transcriptions (benchmark/cpptrace Linux define sets) are made once on keres
against the reference CMake configure, per the wave-1 parity protocol.
`windows` atoms are accepted-but-false on both campaign platforms (validated,
never expanded) — cfg is deliberately ahead of toolchain support there.

### 2.5 Migration notes

- **ninja**: manifest stops being a posix projection (win32/posix split +
  NOMINMAX declarable); `USE_PPOLL` under `cfg.linux` fixes the silent
  pselect fallback on keres.
- **abseil**: `-lrt` (`base`), `-pthread` (`synchronization`, until §5.4's
  Threads edge replaces it), CoreFoundation — all correctly scoped; three-line
  generator diff.
- **benchmark**: macOS/Linux `HAVE_*` sets split under `cfg.macos`/`cfg.linux`;
  `-lrt`/`shlwapi.lib` declarable. Solaris kstat stays a comment
  (out-of-vocabulary, honest).
- **cpptrace / cppcheck**: define trees become labeled transcriptions;
  `HAVE_EXECINFO_H` carries the musl caveat in its comment.
- **vtz / googletest**: clang-gated warning flags under `cfg.clang` — the
  gcc-on-keres acceptance case.

---

## 3. Testing (B2) — decided: T2 (orthogonal markers) + shared substrate

### 3.1 Surface

```toml
[dev-dependencies]                        # grammar identical to [dependencies]
googletest = { git = "https://github.com/google/googletest", tag = "v1.17.0", find-package = "GTest" }

[targets.tests]
type = "executable"
test = true                               # implies dev = true; runner-registered
sources = ["src/expander_test.cpp"]
dependencies = ["json-tui-lib", "GTest::gtest_main"]

[targets.vtz_testing]
type = "static-library"
dev  = true                               # dev graph; not runnable

[targets.bench_vtz]
type = "executable"
dev  = true                               # dev graph, deliberately not a test

[[targets.test_tzdb_load.run]]            # N entries = N invocations
name           = "death-bad-env-path"
cwd            = "tzdb-runtime"
env            = { VTZ_TZDATA_PATH = "/bad/env/path" }
env-remove     = ["TZ"]
expect-failure = true
```

### 3.2 Semantics

- **`dev = true`** (any kind): dev-graph membership — may reference dev-dep
  targets and other dev targets; excluded from the default build and from
  export; buildable by explicit name. **`test = true`** (executables only):
  implies `dev`; registers with the runner. Edge rule, the whole model:
  **a non-dev target may not depend on a dev target or a dev-dep-owned
  target; every other direction is legal.** Markers are non-defaultable
  (§7.2), non-cfg-conditional (§2), and `test = true, dev = false` is a
  hard error (dual-role shipped-and-smoke-tested binaries: reserved, on
  benchmark-suite evidence).
- **`[dev-dependencies]`**: one `DependencySpec`, two tables; single
  resolution namespace; key collision across tables → load error; a regular
  dep's `needs` naming a dev-dep → hard error; `patches`/`system = true`
  legal with identical semantics (§5). Export (§6) excludes dev targets and
  dev-deps unconditionally.
- **Locking is eager, provisioning is lazy** **[coherence ruling —
  overrules the testing designer's lean]**: `cpp-pkg` resolves and locks
  every declared dependency, dev and cfg-inactive alike — CppPkg.lock is the
  complete declared universe, platform- and dev-independent (the same rule
  cfg already fixed for inactive branches; also Cargo's contract). System
  deps lock as machine-independent *declarations*; probing this machine is
  provisioning, not locking (§5.3). Fetch,
  build, and probe of dev-deps happen only when the requested set contains a
  dev target reaching them — `cpp-pkg build` of json-tui does no store work
  for googletest, and with a committed lockfile, no network either.
- **`[[targets.<t>.run]]`** (legal only when `test = true`): fields `name`
  (optional, unique per target), `args`, `cwd`, `env`, `env-remove`,
  `expect-failure` (default false). Zero entries = one default invocation
  (no args, project-root cwd, inherited env). Interpolation per §0.3
  (`${project-root}`, `${build-dir}`, `${gen}`).
- **Runner** (`cpp-pkg test [FILTER...] [--config] [--toolchain] [--jobs N]
  [--list] [--verbose] [-- PASSTHROUGH...]`): builds selected test targets
  via the ordinary pipeline, then spawns each invocation directly (argv
  array, **no shell**), **serial by default** (`--jobs` opt-in; shared
  fixture cwds — vtz — make parallel the unsafe default), captured output
  replayed on failure, `--` passthrough appended to every selected
  invocation. FILTER matching nothing → hard error; no tests declared →
  "no test targets", exit 0.
- **Pass criterion**: exit 0 ⇒ pass. `expect-failure = true`: pass iff
  nonzero exit **or signal death** (shell semantics — matches vtz's actual
  `test $? -ne 0` check; `expect-signal` reserved if CTest-strict semantics
  are ever needed).
- **cwd rule**: relative to project root; created iff it resolves inside
  the **top-level build tree** (`build/` — deliberately the whole tree,
  not the per-config subdir `build/<config>/` that Appendix A.10
  introduces, so this rule is stable across that change and ninja's
  `cwd = "build/test-scratch"` stays auto-created); otherwise must exist —
  a missing fixture cwd fails that invocation legibly and the suite
  continues.
- **env rule**: inherit → apply `env-remove` → apply `env`. Set-to-`""` and
  remove are distinct (CMake `ENVIRONMENT_MODIFICATION` parity, vtz).
- **Default build set**: `cpp-pkg build` = all non-dev targets. Unmarked
  manifests behave byte-identically to v0. No `--all`-style flag in v1
  (explicit naming suffices; revisit on CI evidence).

### 3.3 Errors

- Visibility violation:
  `error: target 'json-tui' (not a dev target) depends on 'GTest::gtest_main',
  exported by dev-dependency 'googletest'` +
  `hint: mark the target dev/test, or move 'googletest' to [dependencies]`.
- `test = true` on a library → error, hint "libraries use `dev = true`".
- `run` entries on a non-test target; duplicate run names; unknown run
  fields (`deny_unknown_fields`) → errors.

### 3.4 Hashing / lockfile

None. Dev-deps hash, build, and cache exactly like regular deps; lockfile
rows use the existing `[[package]]` grammar unchanged.

### 3.5 Linux behavior

`std::process::Command` argv spawn — no shell dialects; signal
classification via `ExitStatusExt::signal()` (glibc SIGABRT deaths are
*more* common under gcc 16 / `_FORTIFY_SOURCE`; vtz's death tests must pass
identically). Nothing in the surface is platform-conditional; dev-deps add
zero Linux bring-up surface (deferred per-project by laziness).

### 3.6 Migration notes

- **vtz**: the entire README protocol (6 invocations, 774 cases, death
  tests, env matrix) becomes declarative run entries; GTest/date/benchmark/
  absl move to `[dev-dependencies]`; `vtz_testing` `dev = true`; `bench_vtz`
  `dev = true` (not a test — the label would lie); a library consumer stops
  building four dep trees.
- **ninja**: googletest → dev-dep; **`[dependencies]` is empty again**;
  `ninja_test` gets a scratch-cwd run entry.
- **json-tui**: googletest → dev-dep; app-only consumers never touch it.
- **googletest**: 10 samples `test = true`; `cpp-pkg test` replaces the
  shell for-loop; samples leave the default build.
- **benchmark**: suite becomes portable (dev-dep gtest + per-test targets +
  args); output-checked tests remain out of scope (recorded).
- **abseil**: generator emits `dev = true` 1:1 from `TESTONLY`, `test = true`
  per `absl_cc_test`; the "impossible" full port loses its testing blocker.
- **cppcheck / cpptrace**: `testrunner` and unit tests marked; cppcheck's
  suite additionally needs §7.1 (cli lib) and §6.5 (runtime-data declared
  on **both** `cppcheck` and `testrunner` — legal via §6.5's byte-equal
  dedupe, so `cpp-pkg test`, which builds only `testrunner`, still stages
  `cfg/`/`platforms/` beside it).

---

## 4. Codegen (B4) — decided: Candidate 1, `checked-in` mode included

### 4.1 Surface

```toml
# tier a — template substitution: pure function of manifest + lockfile
[generate.version-header]
template = "src/version.hpp.in"           # @VAR@ substitution
output   = "src/version.hpp"              # lands at ${gen}/src/version.hpp
vars     = { CMAKE_PROJECT_VERSION = "${package.version}" }

# tier b — declared command, a sandboxed ninja edge
[generate.browse-py-h]
command = ["sh", "src/inline.sh", "kBrowsePy"]
stdin   = "src/browse.py"                 # auto-input
stdout  = "build/browse_py.h"             # auto-output, under ${gen}
inputs  = ["src/inline.sh"]

# checked-in mode — blessed committed-generated pattern (vtz, ninja re2c)
[generate.known-zones]
command    = ["python3", "scripts/gen_known_zones.py", "data/tzdata/tzdata.zi"]
stdout     = "known_zones.h"
inputs     = ["scripts/gen_known_zones.py", "data/tzdata/tzdata.zi"]
checked-in = "include/impl/vtz/known_zones.h"

[targets.ninja]
includes = { private = ["${gen}"] }
```

Exactly one of `template`/`command` per step (else hard error). Step names
share the target charset. No ordering fields: a `${gen}` input that is
another step's output creates the edge implicitly; cycles → plan error.

### 4.2 Semantics (adopted from candidates/codegen.md §1–2; key rulings)

- **`${gen}` = `build/gen/`**, single root (per-step roots deferred).
  Output paths are relative; `..`/absolute → hard error — **source-tree
  writes are refused by construction**. Output collisions → plan-time hard
  error, **case-insensitive on all platforms** (a macOS-authored manifest
  cannot mean two files on Linux).
- Template semantics: `@VAR@` only (`@ONLY` parity); unbound token → hard
  error listing the template line (never CMake's silent empty); unused var →
  warning; `#cmakedefine`/`01` → hard error "not supported in v1".
- Generated headers join targets via `${gen}` in `includes` (**always `-I`,
  never `-isystem`** — your generated code is your code); generated sources
  must match a declared output byte-for-byte (no globbing under `${gen}`).
  Compile units referencing `${gen}` get an order-only dep on the phony
  `cppkg-gen` aggregate; depfiles give exact precision from build 2.
- **Laziness**: a step runs only when the requested build set (or a test
  run) references `${gen}` — with §3's default build, fixture-only steps
  (vtz zoneinfo) run for `cpp-pkg test` only, for free.
- **Input validation is scoped to the activated step set** [coherence
  ruling]: a missing declared input is a plan-time hard error naming the
  path for steps the current invocation *activates* (per the laziness
  rule), never for dormant ones. `cpp-pkg build vtz` from a fresh clone
  activates zero steps and must succeed from a pure source tree (vtz
  GAPS §1 — v0 behavior preserved); `cpp-pkg test vtz` activates the
  zoneinfo step and fails loudly on the missing tzdata, naming the fetch
  script. `checked-in` steps live outside the build graph (below) and
  validate their inputs only under `cpp-pkg gen` / `gen --check` —
  `build` compiles the committed file with no questions asked.
- **Hermeticity**: `command` is argv (no shell), executed via the hidden
  `cpp-pkg gen-exec` wrapper: cwd = project root; **no network — policy
  normative on both platforms, enforcement best-effort on both**: Linux
  attempts `unshare -n` (namespace-enforced when available); where
  unprivileged user namespaces are unavailable (docker/CI defaults), the
  step still runs un-sandboxed and cpp-pkg prints one warning per
  invocation naming the degradation — parity with macOS's best-effort
  `sandbox-exec`. A sandbox's *failure to spawn* is never a build
  failure; a sandboxed step's network attempt failing loudly is the
  enforcement working. Declared outputs
  verified after exit 0; temp-write + atomic commit, mtime preserved on
  unchanged bytes (restat-friendly). Undeclared reads undetected in v1;
  host tool identity unhashed (same hole CMake has — documented).
- **`checked-in` steps** are outside the build graph: `cpp-pkg build`
  compiles the committed file; **`cpp-pkg gen`** regenerates and copies over
  the checked-in path (the one sanctioned source-tree write, via an explicit
  verb); **`cpp-pkg gen --check`** byte-diffs (CI mode — vtz's
  `--verify-refresh`, productized).
- **Interpolated defines are accepted** (codegen OQ1 — decided yes): this is
  what lets S1 keep per-source flags deferred. benchmark's fix is a define,
  not a step:
  `defines = { private = ['BENCHMARK_VERSION="v${package.version}"'] }`.
- `${pin.self.*}`: reserved; root-build = hard error when it lands (§0.3).
- Tier c (per-source transform — cppcheck matchcompiler) and tier d (pinned
  asset fetch — vtz tzdb) stay **deferred**; interim contracts: matchcompiler
  via `USE_MATCHCOMPILER=Off` (behavior-identical by upstream's own Verify
  contract), asset fetch stays in scripts, and missing `[generate]` inputs
  are a hard error naming the path *when their step activates* (see the
  validation-scope ruling above) — the script dependency is loud exactly
  when something actually needs its output, never on a codegen-free build.

### 4.3 Hashing

None in v1 (deps build via CMake; `[generate]` never executes in the store).
Recorded for B6's CppPkg-native dep mode: the `[generate]` table folds into
the config hash as template bytes + post-interpolation vars + argv +
declared-input content hashes.

### 4.4 Linux behavior

Tier a is pure Rust — bit-identical by construction. Tier b argv edges run
identically under ninja on Arch (`sh`, `python3`, `zic` all present);
sandboxing is *stronger* on Linux when user namespaces are available
(§4.2's warn-and-degrade fallback covers containerized CI). Case-insensitive
collision checks keep
manifests portable in both directions. No platform truth is encoded in
`[generate]` — zero new macOS-projection surface.

### 4.5 Migration notes

- **json-tui**: pin.sh codegen block deleted; version stated once
  (`${package.version}`); both consumers (`json-tui-lib`, `tests`) reference
  `${gen}/src` — the multi-consumer case that killed Candidate 2.
- **cpptrace**: three-`sed` pin.sh block → one template step; generated
  `version.hpp` is a **public** `${gen}` include (exports via §6 with zero
  extra words).
- **benchmark**: hardcoded version define → interpolated define; the
  git-describe/store-strips-`.git` trap dissolves with **no step at all**;
  `${pin.self.requested}` upgrades it in dependency mode when implemented.
- **ninja**: browse_py.h becomes a real edge (silent staleness dead); re2c
  becomes two `checked-in` steps with `gen --check` as the drift guard.
- **vtz**: zic → tier-b step feeding test env via `${gen}/zoneinfo`; two
  checked-in headers → `checked-in` steps; hand-rolled `--verify-refresh`
  retired. tzdb *fetch* stays scripted (tier d, deferred, loud-on-missing).

---

## 5. Dependency patches + system dependencies (B5 + B7) — decided: Candidate A

### 5.1 Surface

```toml
[dependencies.absl]
git     = "https://github.com/abseil/abseil-cpp"
rev     = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"     # = tag 20260107.1
patches = ["patches/absl-0001-mark-heterogeneous_lookup_testing-TESTONLY.patch"]
options = { ABSL_ENABLE_INSTALL = "ON", ABSL_PROPAGATE_CXX_STD = "ON" }

[dependencies.zstd]
system      = true            # resolve from the machine, never build
min-version = "1.5"           # optional

[targets.bench]
dependencies = { private = ["Threads::Threads"] }        # builtin, no declaration
```

### 5.2 `patches`

- Array of paths relative to the manifest's directory; bare strings only in
  v1 (the `{ file, strip }` table form is **reserved**, consistent with the
  deps-array string-or-table precedent; no wave-1 evidence needs
  `strip ≠ 1`). Missing file at resolve → error naming dep + path;
  duplicates → error.
- Valid on `git`/`url` sources in both dep tables; invalid on
  `system = true` ("system dependencies have no source tree to patch").
- Applied after fetch/extract, **before** the B12 `subdir` root is entered
  and before configure, in manifest order, via
  `git apply -p1 --whitespace=nowarn` at the checkout root (works in
  non-repo dirs → url tarballs share the code path). Exact context, zero
  fuzz, offset drift tolerated; `../` escapes rejected by git apply; binary
  patches allowed. Any hunk failure → hard error citing dep, patch file,
  failed hunk, with hint `re-diff against <resolved commit>`. Apply into a
  temp dir, rename on success (atomic; pristine raw entry never mutated).
- **Hash spine** (the fix for the observed per-machine `fee068f7` vs
  `f4632513` split):

  ```
  package_id = <base> "+patches:" blake3_32( for each patch, in order:
                                             u64-LE(len) || raw bytes )
  ```

  Patches fold into the **package id**, not a new `ConfigHashInputs` field —
  patched sources *are* different sources; the `cppkg-config-hash-v1`
  encoding is untouched and **no existing store entry is invalidated**.
  `store::raw_dir` incorporates a distinguishable patch suffix
  (`absl-255c84da+a1b2c3d4`). Renaming a patch file without changing bytes
  does not re-key (bytes are hashed, not names).
- **Lockfile** (lock ABI addition): `patches = ["blake3:<hex>", ...]` rows
  in application order, present iff declared; resolve re-verifies against
  current patch files, drift → re-resolve → rebuild, by design.
- `${pin.<dep>.commit}` reports the **base commit**, never the composed id
  (version-stamping wants upstream identity).

### 5.3 `system = true`

- A third source form in the one `[dependencies]` table (mutually exclusive
  with `git`/`url`, `patches`, `options`; `needs` **on** a system dep is an
  error; other deps may `needs` it and targets may reference its exported
  targets — unchanged plumbing).
- **The lockfile records the declaration, never the machine** **[coherence
  ruling — overrules the winning candidate's resolved-version lock row]**:
  a system dep's lock row is `source = "system"` + key + declared
  constraints (`min-version`; the reserved `pkg-config` name when it
  lands). Machine facts — resolved version, paths, file hashes — live only
  in the machine-local sysdep store entry (`cppkg-sysdep-v1`, below),
  never in CppPkg.lock. Anything else makes the lockfile churn per
  machine (uncommittable), and makes a `[cfg.linux]` sysdep unlockable
  from macOS. §3.2's "locking is eager, provisioning is lazy" applies
  with the machine probe classified as *provisioning*: it runs only when
  the requested target set reaches the sysdep under an active predicate.
  An uninstalled system dep errors at need-time, not resolve-time —
  cppcheck's `boost = { system = true }` leaves every non-Boost target
  buildable on a Boost-less machine, matching the green v0 port.
- **Resolution mode v1 = "cmake"**: the tier-2 probe runs
  `find_package(<find-package or key>)` with the hermetic find restrictions
  opened for exactly this package. Result is a **manifest-only store entry**
  (no artifacts), flagged system, re-resolved when recorded file hashes no
  longer match disk. A **`pkg-config = "<name>"` field is reserved, not
  implemented** — recorded semantics: switches that entry's resolution to
  pkg-config; Arrow (whose bundled-vs-system toggles are its central idiom)
  is the designated evidence source before shipping it. Consequence
  accepted: cppcheck's PCRE (`HAVE_RULES`) stays explicitly deferred.
- **Sysdep hash** (new domain tag `cppkg-sysdep-v1`): key, resolution mode,
  resolved version string, sorted resolved library paths, **blake3 of each
  library file's bytes**, sorted include dirs. Enters dependents'
  `dep_hashes` via existing plumbing. Header trees are not hashed
  (documented gap). Store entries downstream of a system dep are
  machine-local **by construction** — different keys instead of today's
  different-artifacts-same-key lie. The CLI names the sysdep explicitly when
  its hash mismatch triggers invalidation ("your OS updated libzstd").
- Not-found error offers both worlds:
  `declare it as a fetched dependency (git/url) to build hermetically, or
  install <name> (pacman -S zstd / brew install zstd)`.
- **Bundled-vs-system per platform is inexpressible in v1** — stated
  plainly: §2.2's one-declaration-per-key rule makes the two-branch
  spelling (`[cfg.linux.dependencies.zstd] system = true` plus a fetched
  entry on another branch) a hard error, and no other spelling exists. In
  v1 a dep is bundled everywhere or system everywhere; a *single*
  cfg-conditional declaration is fine (a linux-only sysdep works today).
  Relaxing one-declaration-per-key for mutually exclusive branches is the
  recorded revisit point when Arrow (whose bundled-vs-system toggles are
  its central idiom) supplies the evidence.

### 5.4 Builtin pseudo-package: `Threads::Threads`

- v1 builtin list: Threads only (`dl`/`m`/`rt` are cfg link-flags until
  evidence). Spelling stays the CMake-shaped `Threads::Threads`
  (extraction-identical; C++-native familiarity).
- Referenceable with no declaration (ladder step 0, §0.6);
  `exposes-targets = ["Threads::Threads"]` → **hard error** whose message is
  the fix ("builtin pseudo-package; delete this line"). Three wave-1
  manifests carry the line. **Explicit decision — this is the wave's only
  flag-day incompatibility (logged in the morning report):** hard error,
  no warn-first release. Rationale: the message *is* the complete one-line
  fix; all three affected manifests are in-repo and re-edited in the same
  S4 wave; and a warned-but-tolerated claim to expose a builtin is a
  manifest that lies (tie-breaker 1). Warn-first (patches-sysdeps OQ9's
  alternative) protects an out-of-repo manifest population that does not
  exist yet; adopt a real deprecation policy when one does.
- Expansion is a pure function of toolchain identity (already hashed —
  **zero new hash inputs**), keyed to the §2.1 `os` axis, **not** the
  triple's libc field: os = linux (gnu *and* musl — musl wants `-pthread`
  equally) → `-pthread` on compile and link of
  every target whose closure contains it; darwin/msvc → nothing. Propagates
  as a usage requirement (CMake parity). Emitted before target flags so
  last-wins user overrides hold.
- Extraction side: imported `Threads::Threads` in probe output is dropped
  from ownership attribution and rewritten to the symbolic link input
  `builtin:threads`; unexpected extracted shapes → warning, literal
  interface kept. Store manifests become *more* platform-portable.
  Cached manifests written by older extractors converge via the
  extractor-version manifest re-derivation (Appendix A.8) — warm stores
  are not allowed to disagree with fresh machines.

### 5.5 Hermeticity scan

- Invariant policed (not a hash input): *every absolute path in a store
  manifest is covered by some hash input* — store-rooted by `dep_hashes`,
  declared-system by the sysdep hash, anything else is a lie.
- Two layers: (1) the manifest post-pass over link libraries / include
  dirs / framework paths — run on probe output **and on every cached
  manifest at ingestion** (cpptrace's already-leaked zstd store entry
  must fire on the next build that reads it, not only on fresh
  extraction); (2) `check_find_package_leaks` extended to
  `*_LIBRARY`/`*_INCLUDE_DIR` cache shapes (the find_library route cpptrace's
  zstd leak used), with declared-sysdep allowlist.
- **Error by default.** Message names the dep, the leaked path, and both
  fixes ("declare `[dependencies.zstd] system = true`, or disable the
  feature"). CLI downgrade: `cpp-pkg build --allow-undeclared-system-libs`
  (warn; documented unsupported-for-sharing). **No manifest knob** — a
  manifest permanently declaring "I lie" fails tie-breaker 1. SDK-rooted
  paths are not exempt.

### 5.6 Linux behavior

Patches byte-identical. Threads → `-pthread` on os = linux fixes every
wave-1 manifest's silently missing edge (correct for merged-libpthread
glibc ≥ 2.34, older glibc, and musl alike — §5.4). Arch is the friendly sysdep target (one universe
under `/usr`, configs/versions present). The scan matters *more* on Linux,
where `/usr/lib` hits are the default failure mode — it will fire on day
one of bring-up; that is it working.

### 5.7 Migration notes

- **vtz**: absl patch — pin.sh clone/sed machinery (40 lines,
  `@ABSL_PATCHED_REPO@`, per-machine synthetic commits) → a 4-line upstream
  dep + one committed patch file; manifest buildable as checked in;
  lockfile committable. date patch **deleted outright** (tool fix 2,
  Appendix A). `exposes-targets = ["Threads::Threads"]` deleted (now an
  error). The absl patch later moves to `[dev-dependencies]` (§3) with
  unchanged meaning and hash.
- **abseil consumer**: the 20260526.0 self-edge blocker → **no patch**
  (tool fix 1); dep is two lines of upstream truth.
- **cpptrace**: the silent Homebrew-zstd leak becomes a choice between two
  honest spellings — fetched zstd (`subdir = "build/cmake"`, B12) or
  `system = true`; the silent third option now errors.
- **cppcheck**: ambient Boost → `system = true` + `USE_BOOST = "On"`
  (declared, not ambient); PCRE/`HAVE_RULES` explicitly deferred (reserved
  `pkg-config` field is its designated future).
- **benchmark / ninja / googletest consumers**: gain the upstream
  `Threads::Threads` edge they currently drop.

---

## 6. Install & export (B6) — decided: Candidate B on the shared spine

### 6.1 Surface

```toml
[package]
name    = "vtz"
version = "1.4.0"                 # required to export a library (SameMajorVersion)

[export]                          # optional; defaults shown
cmake-name = "vtz"                # default = package.name
namespace  = "vtz"                # default = package.name

[targets.vtz]
type    = "static-library"
includes = { public = ["include/api"], private = ["include/impl"] }
install = true
# headers DERIVED: everything header-shaped under include/api installs.
# Total override for the exceptional layout (abseil's repo-root include):
# public-headers = { base = ".", patterns = ["absl/**/*.h", "absl/**/*.inc"] }

[targets.vtz-tldr]
type    = "executable"
install = true                    # → <prefix>/bin/vtz-tldr

[targets.cppcheck]
runtime-data = [
  { from = "cfg",       patterns = ["*.cfg"] },
  { from = "platforms", patterns = ["*.xml", "!*-unsigned.xml"] },
  { from = "addons" },            # whole dir; default to = last path component
]
defines = { private = ['FILESDIR="${install-prefix}/share/cppcheck"'] }
```

### 6.2 The verb and layout

`cpp-pkg install --prefix <dir> [--destdir <dir>] [--config] [--toolchain]
[--list] [targets...]` — builds, then stages FHS layout
(`bin/`, `lib/`, `include/`, `lib/cmake/<CmakeName>/`, `share/<package>/`).
`--destdir` (and `DESTDIR` env) stages into `<destdir><prefix>` while
baked-in paths refer to `<prefix>` — the distro-packaging contract, day one.
`--list` prints the full staging plan without writing (**ships in v1** — it
is the audit tool that makes derived headers reviewable). Idempotent,
overwrite-by-default, never deletes what it didn't just write; no uninstall.

### 6.3 Export emission and the fixpoint

The existing `shim.rs` emitter pointed at the project's own `[targets]`
(exported subset):

1. `<CmakeName>Config.cmake` — IMPORTED targets under `namespace::`;
   **relocatable** (`_IMPORT_PREFIX` pattern, never absolute); public
   defines/flags/cxx-std as `INTERFACE_COMPILE_DEFINITIONS/OPTIONS/FEATURES`;
   private static-lib deps as `$<LINK_ONLY:...>` — the property spelling
   `probe.rs` already reads.
2. `<CmakeName>ConfigVersion.cmake` — **SameMajorVersion** from
   `[package].version`; exporting a library with no version → hard error;
   binaries-only installs need none.
3. `cppkg-manifest.json` — the manifest beside the Config, paths against
   `@prefix@` (CPS precedent). A future `prefix`-form dependency (deferred,
   split out) reads it directly.

**Fixpoint invariant (acceptance test):** probing the installed Config with
the tier-2 probe reproduces `cppkg-manifest.json` exactly (modulo prefix) —
extends the existing `shim_roundtrip_cmake_properties` test to local targets.

### 6.4 Header derivation (why B)

For each exported library, every file under each `includes.public` dir
(including `${gen}` public dirs) matching the header-extension set
(`.h .hpp .hh .hxx .inc .ipp`) installs to `include/<rel-path>`. The
declared public interface **is** what ships — it cannot desync from include
claims, even under cfg projections (whatever a cfg branch appended to
`includes.public` is what installs on that platform).

- `public-headers` (same shape as Candidate A's) is a **total override**,
  never merged — the answer to "what installs?" is always exactly one of
  derived/declared. Consequently it is not cfg-conditionable (§2.2;
  condition `includes.public` and let derivation follow). `${gen}` is
  **not** whitelisted in `public-headers.base` or `runtime-data.from` in
  v1 (§0.3's position table is exhaustive): generated public headers ship
  via the derivation path — `${gen}` dirs in `includes.public` — which is
  wave 1's only generated-header export case (cpptrace). Wave 1 needs the
  override once (abseil).
- Errors: exported library whose derivation is empty → hard error naming the
  dir; same-`include/`-path different-bytes collisions → hard error;
  byte-equal overlaps dedupe; symlinks not followed.
- Export closure rules: unexported local target in an exported closure →
  hard error (`add install = true to 'X' or remove the edge`); dev targets /
  dev-deps in an exported closure → hard error; `install = true` on a
  `test = true` target → error. External deps → `find_dependency(...)` in
  the Config **and** `requires` rows (source URL + pin + options **+
  patches**) in `cppkg-manifest.json`. A patched dep's row carries its
  ordered patch blake3 ids (§5.2's lockfile spelling), and the patch
  bytes themselves are staged into the prefix at
  `lib/cmake/<CmakeName>/patches/<blake3-hex>.patch` — the spine's
  promise ("re-provision the *identical* dependency from the recorded
  pin") is unkeepable from the pin alone when the producer patched it
  (vtz-absl shape: the unpatched tree may not even probe). A consumer
  re-applies by hash order; a `requires` row citing a patch id whose
  bytes are absent from the prefix → hard error at consume time. System
  deps (§5.3) serialize as system requirements
  from their store entries, never as resolved paths. Static-closure
  vendoring (cpptrace upstream's bundled `libdwarf.a`): **deferred**,
  documented divergence.
- Exported manifests may not contain absolute paths (one more hermeticity
  rule, §5.5).

### 6.5 Runtime data and `${install-prefix}` (the cppcheck fix; ships first)

`runtime-data` (fields: `from` required — missing dir is a hard error;
`patterns` per §0.4, default `**/*`; `to` default = last component of
`from`): staged at **build time** next to the target's output via ninja copy
edges (order-only attached, so `cpp-pkg build cppcheck` always stages —
kills the silent zero-findings shape), and at install time under
`share/<package>/<to>/`. Destination collisions: **byte-equal sources
dedupe** — the same rule header overlaps already get in §6.4 — so two
targets may declare the same data and share the staged copies (cppcheck's
`cppcheck` + `testrunner` both declare the `cfg`/`platforms` sets; each
binary gets the data beside it, so `cpp-pkg test`, which builds only
`testrunner`, still stages — without dedupe the only legal spellings were
a collision error or a single-target declaration that re-creates the
zero-findings failure inside the runner). Different bytes for one
destination → hard error.

`${install-prefix}` in define values (§0.3): changing the prefix rebuilds
exactly the TUs embedding it; deps and store keys untouched. Dev-tree
behavior is honest by composition (baked path absent → tool's exe-dir
fallback → build-tree staging is there).

### 6.6 Out of scope in v1 (honest)

No `.pc` emission (recorded Linux-ecosystem cost; cheap later from the same
manifest — wave-2 evidence decides), no shared libraries / SONAME, no
CPack-style packaging (DESTDIR is the packager interface), no IMPORTED
executables in exports (protoc-class tool packages: reserved question).

### 6.7 Linux behavior

FHS + DESTDIR + relative Configs are the distro contract; staging is plain
`std::fs`. keres validation: install ninja + cppcheck into a scratch prefix
and run *from the prefix* (cppcheck's cfg lookup is the sharpest
cross-platform test); install vtz and build upstream's `date_util_example`
against the emitted Config with gcc 16.

### 6.8 Migration notes

- **ninja / json-tui**: `install = true` on the one product executable —
  one line each; `bin/ninja` exists at last.
- **cppcheck**: `stage-data.sh` deleted (runtime-data); hardcoded
  `FILESDIR="/usr/local/share/Cppcheck"` → `${install-prefix}` define;
  build tree stops being silently broken on first build.
- **vtz**: `install = true` — the FILE_SET api/impl split maps exactly to
  `includes.public = ["include/api"]`, zero extra words; upstream's
  `date_util_example` consumer works via `find_package(vtz)`.
- **googletest**: `[export] cmake-name = "GTest" namespace = "GTest"` —
  adopting CppPkg.toml no longer orphans the `find_package(GTest)`
  ecosystem; migration mode (b) re-consumes our own emission.
- **benchmark**: install + SameMajorVersion ConfigVersion; `.pc` files are
  the recorded loss; fixpoint test named in its GAPS runs verbatim.
- **cpptrace**: generated `version.hpp` installs via its public `${gen}`
  include with zero words; libdwarf becomes `find_dependency` + pins
  instead of a vendored archive (documented divergence).
- **abseil**: with §7.2, `install = true` + the one `public-headers`
  override + `namespace = "absl"` written once — 93 targets export without
  93 repetitions; the port stops being "a cul-de-sac artifact-wise".

---

## 7. Ratified small surfaces

### 7.1 Glob exclusion (B8)

`!`-negative patterns in `sources` (grammar per §0.4):

```toml
[targets.cli-lib]
sources = ["cli/*.cpp", "!cli/main.cpp"]        # cppcheck's cli library, at last
```

Dissolves: cppcheck's unreproducible cli library (testrunner can now share
it), benchmark's 19 hand-listed files (`["src/*.cc",
"!src/benchmark_main.cc"]` — new upstream files stop being silent link
errors), vtz's 7-file bench list. ninja's win32 split is §2's job, correctly
not this. ~30 lines in `graph.rs`; no hashing/store interaction.

### 7.2 Target defaults (B9)

```toml
[target-defaults]
cxx-std = 17
defines = { private = ["HAVE_RULES=0", ...] }
install = true                # abseil: with public-headers override;
                              # fills eligible targets only (skips dev/test)
```

- Accepted keys v1: `cxx-std`, `c-std`, `defines`, `includes`,
  `system-includes`, `install`, `public-headers`, `runtime-data`.
- **Excluded**: `dependencies` (inherited edges make graphs unreadable),
  `dev`/`test` markers and `run` arrays (a default that reclassifies graph
  membership lies), `sources`, `type`.
- **Excluded in v1, reserved** **[coherence ruling]**: `cxx-flags`/`c-flags`/
  `link-flags` — `[flags]` is the single home for "flags every target gets";
  a second home at a different layer position was the fragmentation risk.
  Error message points at `[flags]`. Revisit only on wave-2 evidence of
  per-target-defaultable flags that `[flags]` cannot express.
- Merge per §0.5 (scalars fill-if-absent; lists prepend). No per-key opt-out
  in v1; subtraction demands are the recorded revisit trigger.
  `[target-defaults.cfg.<pred>]` reserved (§0.2).
- **Eligibility, not opt-out**: a default never fills a key onto a target
  where the key is categorically illegal for that target's markers/kind.
  Concretely: `install` and `public-headers` skip `dev`/`test` targets
  (§3.2 excludes them from export; §6.4 makes `install = true` on them a
  hard error — a default must not manufacture that error at scale, and
  hand-writing `install = false` per dev target would recreate the
  repetition B9 exists to kill); `public-headers` additionally fills only
  onto libraries that (effectively) have `install = true`. `runtime-data`
  fills onto every target — build-time staging is exactly what test
  runners want (§6.5), and multi-target staging is legal via §6.5's
  byte-equal dedupe. Eligibility is decided from the target's own
  (pre-merge) markers and kind, so the abseil manifest's
  `install = true` default reaches the shipped libraries and skips all
  241 test executables and 40 TESTONLY libs without a single per-target
  line.
- Merge happens at schema load, before validation, so errors point at
  effective values; `cpp-pkg build --query` shows results.
- Dissolves: abseil's measured 29% (2 lines × 93), cppcheck's
  4-defines+`cxx-std` × 5 ("single ugliest thing"), googletest's
  `cxx-std = 17` × 14 silent-default hazard, benchmark/json-tui directory
  defines. Target templating and `targets-from` stay deferred (additive).

---

## 8. Config-hash impact summary (one table for implementers)

| Feature | Hash impact |
|---|---|
| target flags, non-ABI `[flags]` | none (asserted by test) |
| ABI-classified `[flags]` entries | dependency config hashes, via existing profile-ABI rule |
| cfg | none (project-side; all deps locked, only active built) |
| dev-deps / markers / runner | none (dev-deps hash like deps; lockfile grammar unchanged) |
| `[generate]` | none in v1; B6-native-dep folding rule recorded (§4.3) |
| `patches` | composed **package_id** `<base>+patches:blake3_32(len-prefixed bytes)`; encoding `cppkg-config-hash-v1` untouched; lockfile gains `patches` rows |
| `system = true` | new `cppkg-sysdep-v1` domain hash entering `dep_hashes`; machine-local downstream keys by construction |
| `Threads::Threads` builtin | zero new inputs (pure function of hashed toolchain identity) |
| install/export, `${install-prefix}` | none (project TUs only) |
| `subdir` (B12) | hashed as a literal string on the dep (Appendix A) |

No store **artifact** is invalidated by any feature in this wave —
artifact config-hash keys are untouched. Extraction **manifests** are
re-derived once, via the extractor-version component their cache keys gain
in Appendix A.8: a cheap re-probe/re-read, and required — without it the
wave's extractor fixes (`-isystem` classification, the Threads rewrite,
`$<BOOL>` LINK_ONLY) would reach fresh machines but silently never reach
warm stores, recreating the different-content-same-key lie §5.3 condemns.

---

## 9. Reserved & deferred registry (so future rounds stay additive)

**Reserved (distinct error today, spelling fixed):** cfg combinators as
quoted keys (`"all(linux, gcc)"`); probe predicates in the same positions;
`apple-clang` atom; `[target-defaults.cfg.<pred>]`,
`[cfg.<pred>.targets.*]`, `[cfg.<pred>.generate.*]`,
`[profiles.*.cfg.<pred>]`; cfg scalar overrides ("not in v1");
`test = true, dev = false` (dual-role); `expect-signal`; `${pin.self.*}`
(root build = hard error); patch `{ file, strip }` table form;
`pkg-config = "..."` on system deps; flags keys in `[target-defaults]`;
knob sugar (`exceptions`/`rtti`); `frameworks = [...]` field;
`base-config` profiles (pre-existing).

**Deferred (no reserved spelling; needs evidence):** per-source flags;
codegen tiers c (per-source transform) and d (asset fetch, `[assets]`);
`.pc` emission; IMPORTED-executable export; `prefix`-form dependencies;
static-closure vendoring (`bundle`); shared libraries; interface-library
(B10 — wave-2 Boost blocker, not in this wave); whole-archive per-edge
attribute (B11); case discovery / output-matching test harnesses; Windows.

---

## 10. Per-project migration summary (for the S4 re-migration wave)

| Project | Workarounds dissolved by this wave | Remains (deliberate) |
|---|---|---|
| vtz | absl patch → `patches` (later dev-dep); date patch → deleted (tool fix); Threads `exposes-targets` deleted; 774-case protocol → run entries; bench/testing libs → `dev`; zic + 2 checked-in headers → `[generate]` (+ `gen --check`); dev-warning set → `[flags]`; `-Wshorten-64-to-32` → `cfg.clang`; 7-file bench list → `!`; `install = true` | tzdb fetch script (tier d); OBJECT-lib double compile (B11) |
| ninja | gtest → dev-dep (deps empty again); 4× profile stanzas → `[flags]`; browse_py.h → `[generate]`; re2c → `checked-in`; win32/posix + NOMINMAX + USE_PPOLL → cfg; scratch-cwd run entry; `install = true` | AIX/getopt-as-C++ (out of vocabulary); Windows toolchain |
| cppcheck | stage-data.sh → `runtime-data`; FILESDIR → `${install-prefix}`; cli lib → `!`; 4 defines × 5 → `[target-defaults]`; warnings policy → `[flags]`/`[flags.cfg.clang]` + `system-includes`; Boost → `system = true`; HAVE_EXECINFO_H → labeled cfg | matchcompiler (tier c; `USE_MATCHCOMPILER=Off` blessed); PCRE/HAVE_RULES (reserved `pkg-config`) |
| json-tui | gtest → dev-dep; `-Werror` break → `-isystem` fix + target flags (zero suppressions); version header → `[generate]`; profile duplication deleted; `install = true` | — |
| googletest | `find-package` documented; export as `GTest` (ecosystem preserved); `cxx-std` × 14 → defaults; samples → `test = true` + runner; clang-gated warnings → `cfg.clang` | — |
| benchmark | version define → interpolation (git-describe trap dead); HAVE_*/link libs → cfg; 19-file list → `!`; suite → dev-deps + tests; warning battery → `[flags]` + `cfg.clang` split; install + ConfigVersion | output-checked tests; Solaris kstat; `.pc` |
| abseil | self-edge blocker → tool fix (no patch); COPTS → `[flags.cfg.clang]`/`[flags.cfg.gcc]`; LINKOPTS → cfg link-flags; 29% repetition → defaults; TESTONLY/tests → `dev`/`test` 1:1; export `namespace = "absl"` | 54 header-only stub archives (B10, wave 2) |
| cpptrace | version header → `[generate]` (public `${gen}` include exports free); zstd → declared (fetched + `subdir`, or `system = true`) — silent Homebrew leak now errors; per-profile flags → target flags; libdwarf export → `find_dependency` + pins | vendored-archive parity (deferred vendoring) |

---

## Appendix A — Decided tool-fixes batch (no schema surface; same wave)

Prerequisites and papercuts from BACKLOG B1c/B5/B12, all decided, listed so
implementers have one document:

1. **`-isystem` for imported interfaces (B1c)** — imported targets'
   interface include dirs classify into the existing `system_includes`
   bucket (CMake's imported-target default; un-breaks json-tui).
   Classification happens at **manifest ingestion** (the read side),
   normatively — not only at probe time — so cached store manifests get
   the fix without any invalidation. Honor `system-includes = false`
   opt-out (§1.1). Behavior change, and **not** diagnostics-only:
   `-isystem` dirs are searched after all `-I` dirs, so every dep's
   headers move to the end of the include search order — a project header
   shadowing a dep header, or two deps with same-named headers where
   declared order previously decided, can resolve to a different file.
   Release-note both the diagnostic suppression and the search-order
   move, naming the per-dep `system-includes = false` opt-out as the
   escape hatch.
2. **Self-link edges are no-ops (B5-1)** — `absl::strings → absl::strings`
   deduped, CMake's own semantics. Dissolves the abseil 20260526.0 blocker.
3. **Skip non-compilable extensions in extracted INTERFACE_SOURCES (B5-2)**
   — CMake's is-compilable classification; project sources stay strict.
   Dissolves vtz's date patch.
4. **Document the `find-package` dep field** in CPPKG_TOML.md (works,
   undocumented) and translate the probe's raw config-not-found error into
   the existing hint (vtz/abseil/googletest hit the raw error).
5. **`subdir` field on git/url deps**, folded into the config hash as a
   literal string; configure root = `<checkout>/<subdir>`; ordering: fetch →
   patches (§5.2, applied at checkout root) → subdir. Unblocks cpptrace's
   zstd (`build/cmake/`) — the one wave-1 case with **no** workaround.
6. **Submodule guard on actual gitlink entries**, not `.gitmodules`
   presence (json-tui 0-byte false positive).
7. **url deps: accept `.tar.xz` / `.tar.bz2`** (system tar already does).
8. **Evaluate `$<BOOL:...>` inside LINK_ONLY** instead of skipping (and stop
   replaying the skip-notes on cache hits). Required for Linux regardless of
   cfg: without it, extracted abseil silently drops `-lrt` on keres (§2.4).
   This changes extraction *output*, and a warm store would otherwise never
   receive it: store **manifest cache keys gain an extractor-version
   component** this wave. Bumping it re-derives manifests (cheap re-probe /
   re-read; artifacts and their config-hash keys untouched — §8), so warm
   stores converge with fresh machines. Ingestion-time transforms (A.1's
   classification, §5.4's Threads rewrite, §5.5's scan) apply on every
   manifest read regardless; the extractor-version covers the
   probe-output-shaped changes that cannot be re-derived from a cached
   manifest alone.
9. **Translate the CMake ≥ 4 `cmake_minimum_required` refusal** into a
   `CMAKE_POLICY_VERSION_MINIMUM = "3.5"` options hint (will hit every old
   pin).
10. **Misc:** per-config build dirs (cpptrace); lint unknown dep `options`
    keys (cpptrace); flag-ordering last-wins is now normative contract via
    §1.3 (closes the documentation item).

---

## Amendments (architect ratification after the S3 fix pass, 2026-08-14)

1. **§0.3 position table corrected** to include `[generate.*].inputs`/`stdin`
   for `${gen}` (was under-inclusive; §4.1 was and remains normative).
2. **SDK-sysroot allow-list exemption in the hermeticity scan: RATIFIED.**
   §5.5's invariant is satisfied — SDK contents are covered by the hashed
   `sdk_version` in the toolchain identity; the exemption is load-bearing for
   FindZLIB-style SDK .tbd references and inert on Linux. §5.5's contrary
   sentence is superseded by this amendment.
3. **url dependencies lock lazily: RATIFIED.** The declared `sha256` already
   pins content machine-independently; eager locking would require fetching
   bytes at lock time for no integrity gain.
4. **Release notes owed for the next tagged release:** one-time whole-project
   relink (link-rule argv order change, §1.3; store keys untouched);
   transported-ABI flag re-key note from the correctness review.
5. **Recorded open holes (deliberate, tracked):** response-file flag
   laundering not yet classified; per-config build dirs (A.10) deferred.
