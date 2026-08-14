# Design candidates — B1: per-target flags, flag layering, `-isystem` semantics

Status: DRAFT for taste review (S2 design round).
Area: B1 (BACKLOG §2), refining sketch S1. Evidence: json-tui GAPS §4/§4b,
abseil GAPS "per-target-flags", benchmark GAPS §3, cppcheck GAPS §4/§8,
vtz GAPS §4, ninja GAPS §5, googletest GAPS §7/§8, cpptrace GAPS §6.
Normative baseline: CPPKG_TOML.md (v0). Charter: CAMPAIGN.md "Taste charter".

Scope note: this document designs the **schema surface**. The `-isystem`
classification of imported-target interface includes is a **decided tool
fix** (json-tui §4b broke the build; CMake-matching behavior); it appears
here only where it has schema-visible corners. Per-source flags are
**deferred** per the backlog's own analysis (every wave-1 per-source case is
subsumed by codegen B4 or was safely hoisted); this document does not
reopen that.

---

## 0. Shared substrate (identical in all three candidates)

All candidates add the same two scopes and the same layering/propagation
model; they differ **only** in what the target-level surface looks like and
what is allowed to propagate. The substrate is specified once so the
candidates can be compared on the one axis that actually differs.

### 0.1 The profile-independent layer: top-level `[flags]`

```toml
[flags]                       # applies to ALL project targets, ALL profiles
cxx-flags  = ["-Wno-deprecated"]
c-flags    = []
link-flags = []
```

- Same three keys as `[profiles.*]`, same language routing (`cxx-flags` →
  C++ driver only, `c-flags` → C driver only). No visibility split at this
  scope: `[flags]` is build *environment*, not a propagating interface —
  it applies to every project target directly, so propagation would be
  meaningless (everything already has it).
- Consumer-targets-only, exactly like profile flags — with the same single
  exception: entries recognized by the existing ABI classification table
  (`toolchain.rs::is_abi_flag`) are injected into dependency builds and
  folded into each dependency's config hash, byte-for-byte the same
  machinery profiles use today. `[flags]` is deliberately *nothing new
  semantically*: it is "the flags every profile shares", hoisted.
- Ordering (see §0.3): `[flags]` comes before the selected profile's flags,
  so a profile can still override the package layer (last-wins).
- Empty/absent table is fine; unknown keys are a hard error
  (`deny_unknown_fields`, as everywhere).

Rejected spelling — `[profiles.all]`: it reads as a fifth profile but can
never be selected with `--config all`, so the declarative reading would lie
(charter tie-breaker 1). Rejected spelling — `[package] cxx-flags`:
`[package]` is identity metadata; burying build inputs there hides them.
`[flags]` is a new top-level word, but it is the word users grep for.

### 0.2 Target-level keys

Every candidate adds `cxx-flags`, `c-flags`, `link-flags` to
`[targets.*]`, next to `defines`/`includes`. The shape of the value is the
contested surface — see candidates.

Common semantics regardless of shape:

- **Flags are argv words, not shell text.** Each list entry becomes exactly
  one argument (same rule that already carries
  `'FILESDIR="/usr/local/share/Cppcheck"'` through TOML → ninja → shell
  intact). Two-word flags are two entries: `["-Xlinker", "-dead_strip"]`.
- **Last-wins ordering is documented contract** (cppcheck's Release `-O2`
  overriding the built-in `-O3` already relies on it; this design promotes
  it from accident to spec — closing the B12 item "document flag-ordering
  last-wins as contract").
- **`link-flags` on a `static-library`** follow the existing private-dep
  rule: a static library does not link, so its link-flags propagate to the
  final link of every consumer, exactly like private dependencies propagate
  as link-only edges today. (This is also what upstream abseil's LINKOPTS
  do: `target_link_libraries(... PRIVATE ${LINKOPTS})` on a static lib
  reaches the consumer's link via `$<LINK_ONLY>`.) On an `executable`,
  `link-flags` apply to its own link line. Consequence: on static
  libraries the public/private split of `link-flags` is currently
  **observationally identical** (both reach the consumer link; neither has
  compile-time effect); the split becomes meaningful when a shared-library
  kind exists (private = own link only). Documented, not hidden.
- **Sanitizer flags** (`-fsanitize=*`) at any new scope behave as at
  profile scope: consumer-only, with the existing "dependencies are
  uninstrumented" warning.
- **ABI-classified flags at target scope are a hard error**, with a hint:
  "`-stdlib=libc++` affects the ABI of the entire link closure including
  store dependencies; move it to `[flags]` or a `[profiles.*]` block, where
  it will propagate to dependency builds and their config hashes." Target
  scope cannot reach dependency builds, so allowing ABI flags there would
  manufacture silent ABI splits — the one thing the 2026-08-13 ABI decision
  exists to prevent.
- **Lint, not error** (severity is an open question, §OQ3): `-D*`, `-U*`,
  `-I*`, `-isystem`, `-std=*` inside flags lists draw a warning naming the
  dedicated key (`defines` / `includes` / `cxx-std`). Not a hard error:
  migrations paste flag soup, `-UNDEBUG` has no schema home (benchmark's
  tests legitimately need it), and the ABI classifier already catches the
  dangerous `-D_GLIBCXX_*` class wherever it appears.
- **Config hash**: target-level and `[flags]` non-ABI entries never touch
  dependency config hashes (ninja's migration verified consumer profile
  flags already don't). ABI-classified `[flags]` entries do, via the
  existing rule. Nothing else changes.

### 0.3 Layering order (the one true list)

Compile line for a TU of target `T`, left to right (last wins):

1. Toolchain/driver defaults + built-in config flags of the selected
   profile (e.g. `-O3 -DNDEBUG` for release) — unchanged.
2. ABI injection set (ABI-classified entries of `[flags]` + selected
   profile) — unchanged position.
3. `[flags]` (package layer, non-ABI remainder).
4. `[profiles.<selected>]` flags — profile refines package.
5. Public flags propagated from `T`'s transitive compile-visible
   dependencies (candidates A/C only; see §0.4).
6. `T`'s own public flags (A/C) / propagating knobs' expansion (B).
7. `T`'s own private flags — the most specific voice speaks last, so a
   target can override anything above it (benchmark's `donotoptimize_test`
   forcing `-O3` + `-Werror=deprecated-declarations` is exactly step 7
   beating step 4).

Link line: `[flags].link-flags`, then profile `link-flags`, then `T`'s own
`link-flags`, then link-flags collected from the link closure in the same
deterministic order the library inputs already use (dependents before
dependencies), each contributing target contributing its list **once**
(diamonds do not duplicate). Raw `-lfoo` inside `link-flags` is legal but
documented-discouraged: it is not ordered against `$libs`, and system
libraries are B7's problem, not a flag.

### 0.4 Propagation rule (candidates A/C)

Public compile flags propagate like public defines: to every target that
can see `T`'s headers (direct public/private consumers, transitively
through public edges — the existing compile-visibility closure, no new
graph machinery). Collection is in topological order (dependencies before
dependents), **deduplicated by contributing target**, never by flag string
(a target's list is emitted intact once; intentional repeats inside one
list survive; diamond graphs contribute once). This mirrors CMake's
usage-requirement dedup and makes abseil-scale graphs (93 targets, all
publicly chained through `config`) emit each contribution exactly once.

### 0.5 `-isystem` — the schema-visible corners of the decided tool fix

Decided (tool): the tier-2 probe / manifest ingestion classifies imported
targets' interface include directories as **system** by default, so store
dependencies' headers arrive via `-isystem` (toolchain.rs already emits it
for the `system_includes` bucket; the probe just never filled it). This
matches CMake's default for imported targets and un-breaks json-tui's
`-Werror` build without the `-Wno-error=character-conversion` demotion.

Two small schema surfaces fall out, both in scope for this design:

1. **Per-dependency opt-out** — CMake's `NO_SYSTEM_FROM_IMPORTED`
   equivalent, for the user who *wants* to see a dep's header warnings:

   ```toml
   [dependencies.ftxui]
   git = "..."
   tag = "v7.0.0"
   system-includes = false      # default true: interface includes -> -isystem
   ```

2. **Vendored-code parity for project targets** — cppcheck's
   `EXTERNALS_AS_SYSTEM`: an in-tree third-party target can declare that
   its *public* includes are system includes for its consumers (CMake ≥
   3.25 `SYSTEM` target property):

   ```toml
   [targets.tinyxml2]
   type = "static-library"
   sources = ["externals/tinyxml2/*.cpp"]
   includes = { public = ["externals/tinyxml2"] }
   system-includes = true       # default false for project targets
   ```

   The target's own TUs still see them as ordinary `-I`; only consumers
   get `-isystem`. This is one boolean, symmetric with the dep-side knob,
   and it is what makes "warnings-as-errors project with vendored code"
   (cppcheck's exact policy) expressible without per-consumer suppression.

Generated (`${gen}`, B4) and ordinary project includes remain `-I`: your
own code does not get to hide from your own warnings.

### 0.6 What is explicitly out

- Per-source flags/defines (deferred; B4 covers the wave-1 cases).
- Toolchain-conditional flags (`-Wshorten-64-to-32` clang-only): that is a
  *predicate*, and predicates are B3's one job. Requirement lodged with the
  cfg design: **whatever cfg surface wins must accept the flags keys at
  target scope and at `[flags]` scope** (abseil/vtz evidence; see §5).
- "Add flag if the compiler accepts it" (`check_cxx_compiler_flag`): not
  declarative, not in scope. The cfg `compiler` predicate covers the
  family-level 90%; the documented idiom for the rest is clang's
  `-Wno-unknown-warning-option` semantics (cppcheck GAPS §4 sized this as
  covering ~90% of its `_safe` variants).
- LTO/PIC/visibility *presets* (`lto = true` a la cargo): profile-level
  knobs, separable, not blocked by B1. Raw flags express them today.

---

## 1. Candidate A — visibility split everywhere, open strings

The `defines` shape, verbatim, extended to flags. Refines S1-A/C: bare-list
sugar included (it is the existing schema grammar for
includes/defines/dependencies; omitting it here would be the
inconsistency).

### Surface

```toml
[flags]                                   # §0.1, all targets, all profiles
cxx-flags = ["-Wno-deprecated"]

[targets.json-tui-lib]
type    = "static-library"
sources = ["src/*.cpp"]
cxx-flags = { private = ["-Wall", "-Wextra", "-pedantic", "-Werror",
                         "-Wmissing-declarations", "-Wshadow"],
              public  = ["-fno-exceptions"] }
link-flags = ["-Wl,-framework,CoreFoundation"]   # bare list == all-private

[targets.tests]
cxx-flags = ["-Wno-unused-but-set-variable"]     # sugar: private
```

- `cxx-flags` / `c-flags` / `link-flags` on any target; value is
  string-list (sugar for `{ private = [...] }`) or
  `{ public = [...], private = [...] }` — the exact `VisibilitySplit`
  grammar targets already use three times.
- Public bucket: **any string**. Propagates per §0.4.
- Public flags on an `executable` are a validation error ("nothing can
  consume an executable"), same spirit as the existing resolve-time
  hard-error policy.

### Semantics / edge cases

All of §0. Additionally: no policy on public content. `-O3` public is
legal. `-Werror` public is legal — and would recreate json-tui's build
break *by user declaration* (a dep-of-a-dep publishing `-Werror` at you).
The manifest says what happens, so the declarative reading does not lie —
but the footgun the sketch flagged is real and unfenced.

### Error behavior

- ABI flag at target scope → hard error (§0.2).
- Type errors / unknown keys → serde errors naming the key.
- Everything else builds.

### Costs

- The public bucket is an open channel for graph-wide flag injection;
  the first ecosystem package that publishes `-Werror` or `-O0` public
  punishes its consumers, and the tool has opinions about neither.
- Export (B6) must serialize arbitrary public flags into emitted
  Config/manifest shims — fine mechanically, but it makes "what does
  depending on X do to my compile lines" unreviewable in general.
- Cheapest to implement and to explain: one sentence ("flags have the same
  shape as defines").

---

## 2. Candidate B — private-only lists + curated propagating knobs

Refines S1-B. Flags are always private; the *rare* propagating cases get
first-class, ABI-aware names.

### Surface

```toml
[flags]
cxx-flags = ["-Wno-deprecated"]

[targets.json-tui-lib]
type      = "static-library"
sources   = ["src/*.cpp"]
cxx-flags = ["-Wall", "-Wextra", "-pedantic", "-Werror",
             "-Wmissing-declarations", "-Wshadow"]    # always private
link-flags = ["-Wl,-framework,CoreFoundation"]        # always private
exceptions = false                                    # public by definition
```

- `cxx-flags` / `c-flags` / `link-flags`: flat string lists, private,
  period. No table form, no sugar needed (there is nothing to be sugar
  for).
- Curated knob vocabulary, v1 = exactly two: `exceptions = true|false`,
  `rtti = true|false` (absent = toolchain default, nothing emitted). Knobs
  are propagating compile requirements: a knob on a library reaches every
  compile-visible consumer (per §0.4's closure), expanding to
  `-fno-exceptions` / `-fno-rtti` (clang/gcc; MSVC spelling when MSVC
  lands) at step 6 of the layering.
- Knob conflicts are **hard errors at plan time**: if `tests` inherits
  `exceptions = false` from `json-tui-lib` and also declares
  `exceptions = true`, the error names both targets and the edge. (This
  check is the one thing B can do that A/C cannot: the tool understands
  the knob, so it can refuse incoherent graphs instead of last-wins-ing
  through them.)

### Semantics / edge cases

All of §0 minus §0.4 propagation for raw flags (nothing raw propagates).
Honesty note: knobs are consumer-project-scope like all target flags —
store deps are built with default exceptions/RTTI. Mixing
`-fno-exceptions` code with exception-throwing deps is well-defined at the
language level (it is what upstream json-tui does) but the knob does not
and cannot promise dep-side propagation; the doc must say so.

### Error behavior

- A propagating-looking raw flag in a private list (`-fno-exceptions`,
  `-fno-rtti` spelled as strings) → warning suggesting the knob, so the
  conflict checking isn't silently bypassed. Not an error (the flag still
  does exactly what it says, privately).
- Knob conflict → hard error (above). ABI flag at target scope → hard
  error (§0.2).

### Costs

- The vocabulary chases reality forever. Wave-1 needed exactly one public
  flag (`-fno-exceptions`) — B covers the corpus 100% today — but the
  first un-curated public flag (`-fcoroutines`? `-fmodules`?
  `-fchar8_t`? a vendor flag?) has **no escape hatch at all**: the user's
  only out is patching every consumer target by hand, and library authors
  targeting export (B6) simply cannot state their requirement. That is a
  cliff, not a slope.
- Two mechanisms where A has one (charter tie-breaker 3 cuts against B).
- The knobs themselves are good — see Recommendation: they are worth
  having *on top of* an open surface, as sugar with checking, rather than
  *instead of* one.

---

## 3. Candidate C — A's surface, with a propagation-class fence (recommended)

A's grammar, unchanged. One addition: the public bucket is checked against
the flag classifier that **already exists** in `toolchain.rs` (ABI +
sanitizer classes), extended with two more classes. Private stays fully
open; public rejects the classes that are always wrong to propagate.

### Surface

Identical to Candidate A, byte for byte. The difference is entirely in
validation:

```toml
cxx-flags = { private = ["-Wall", "-Werror"],        # anything goes
              public  = ["-fno-exceptions"] }        # classified: OK

# public = ["-Werror"]   -> hard error: warning-class flags do not propagate
# public = ["-O3"]       -> hard error: optimization/debug-class
# public = ["-fsanitize=address"] -> hard error: sanitizer-class
# public = ["-stdlib=libc++"]     -> hard error: ABI-class (already, §0.2)
```

### The classifier (extends `is_abi_flag`'s table)

| class | v1 members | public bucket |
|---|---|---|
| ABI | existing table (`-D_GLIBCXX_*`, `-stdlib=*`, `-f*abi*`, ...) | error (all scopes below profile, per §0.2) |
| sanitizer | `-fsanitize*` | error |
| warning | `-W...` **except** the `-Wl,`/`-Wa,`/`-Wp,` driver pass-through prefixes; `-w` | error: "warnings are private by nature; a library cannot volunteer its consumers into a diagnostic policy" |
| opt/debug | `-O*`, `-g`, `-g[0-9]`, `-ggdb*`, `-glldb*` | error: "optimization level is the consumer's (profile's) decision" |
| everything else | `-f...`, `-m...`, `-pthread`, unknown strings | allowed |

Unknowns default to **allowed** — the fence only blocks the four classes
whose propagation is *categorically* wrong, so the classifier being
incomplete degrades to Candidate A, never to a false rejection. The
corpus's one real public flag (`-fno-exceptions`) passes; every public
footgun the S1 sketch worried about is a compile-time error with a
one-line message. Errors are at manifest-load time (pure string
classification, no toolchain query needed).

`link-flags` public bucket: only the ABI/sanitizer classes are checked
(warning/opt classes don't occur on link lines meaningfully);
`-Wl,...` passes as pass-through class.

### Optional adjunct (taste call, works with A or C): B's knobs as sugar

`exceptions = false` can still exist, defined as *exactly*
`cxx-flags.public += ["-fno-exceptions"]` plus the conflict check. Zero
new semantics — a knob is sugar over the open surface, so the cliff in B
never forms. Deferred by default; listed as OQ2 because json-tui reads
better with it.

### Costs

- The classifier table is now load-bearing for a *rejection* path and must
  be maintained (though only four prefixes deep, and shared with the ABI
  machinery that already exists and already has tests).
- MSVC-style flags (`/W4`, `/O2`) don't match the GCC-shaped prefixes;
  the table gains a spelling per driver family when MSVC lands (bounded,
  and B3's `compiler` predicate is how such flags get scoped anyway).
- Marginally more doc than A: the public bucket has rules. (One table.)

---

## 4. The corpus, before → after

All "after" examples shown in Candidate C's surface (= A's grammar); B
differences noted inline. The tool fix (§0.5) is assumed landed.

### 4.1 json-tui — the build break (GAPS §4, §4b)

Before (workaround, from `migrations/json-tui/CppPkg.toml`):

```toml
[profiles.release]
cxx-flags = [
  "-fno-exceptions",
  "-Wall", "-Wextra", "-pedantic", "-Werror",
  "-Wmissing-declarations", "-Wshadow",
  "-Wno-error=character-conversion",     # global demotion, upstream never needs it
]
[profiles.debug]
cxx-flags = [ # ... identical 8-line block pasted again ... ]
```

After:

```toml
[targets.json-tui-lib]
cxx-flags = { private = ["-Wall", "-Wextra", "-pedantic", "-Werror",
                         "-Wmissing-declarations", "-Wshadow"],
              public  = ["-fno-exceptions"] }

[targets.json-tui]
cxx-flags = ["-Wall", "-Wextra", "-pedantic", "-Werror",
             "-Wmissing-declarations", "-Wshadow"]
```

Both profile blocks deleted. `tests` gets `-fno-exceptions` through the
lib's public flag (upstream's exact structure) and no `-Werror`; gtest
headers arrive `-isystem` regardless, so the
`-Wno-error=character-conversion` demotion is gone with nothing replacing
it. This is upstream's CMake, line for line, in the manifest's own idiom.
(B: `exceptions = false` on the lib instead of the public bucket;
otherwise identical.)

### 4.2 ninja — 4× duplication (GAPS §5)

Before: four identical `[profiles.*] cxx-flags = ["-Wno-deprecated"]`
stanzas covering every `--config` value. After:

```toml
[flags]
cxx-flags = ["-Wno-deprecated"]
```

One line, all profiles, matching upstream's global
`add_compile_options(-Wno-deprecated)` exactly.

### 4.3 abseil — generator scale (GAPS "per-target-flags")

Before: COPTS dropped wholesale; `-Wl,-framework,CoreFoundation` hoisted
into `[profiles.release] link-flags` — every executable links
CoreFoundation, and `--config debug` silently loses it.

After — `header.toml` (hand-written prologue) gains one block:

```toml
[flags]                       # == ABSL_DEFAULT_COPTS, identical on all 93 targets
cxx-flags = ["-Wall", "-Wextra", "-Wcast-qual", "-Wconversion-null",
             "-Wformat-security", "-Wmissing-declarations",
             "-Woverlength-strings", "-Wpointer-arith",
             "-Wundef", "-Wunused-local-typedefs", "-Wunused-result",
             "-Wvarargs", "-Wvla", "-Wwrite-strings"]
```

and `gen_toml.py` emits, for the one target with LINKOPTS:

```toml
[targets.time]
link-flags = ["-Wl,-framework,CoreFoundation"]   # macOS-only: see §5.1 for the
                                                 # cfg form this must grow
```

Because upstream's COPTS are the *same curated list on every target*, the
generator emits **zero** per-target `cxx-flags` lines — the 93-target port
gains correct flags for ~15 lines total, all in the hand-written header.
The private link-flag propagates link-only through the static-lib rule
(§0.2), so exactly the executables whose closure contains `time` link the
framework, in every profile. Per-target divergence, when an upstream
release introduces it, is one generated line on the diverging target —
the generator's diff stays proportional to upstream's diff. (The 29%
`cxx-std`/`includes` repetition is B9's job, not B1's; the two compose but
neither depends on the other.)

### 4.4 cppcheck — vendored-code policy (GAPS §4, §8)

Before: warning policy impossible (`-Weverything` would hit vendored
tinyxml2/simplecpp/picojson; port dropped it, ate ~9 stray warnings, could
not reproduce `EXTERNALS_AS_SYSTEM`). After:

```toml
[flags]
cxx-flags = ["-Weverything", "-Wno-c++98-compat", "-Wno-padded", ...]  # curated -Wno list

[targets.tinyxml2]
system-includes = true                        # EXTERNALS_AS_SYSTEM, per target
cxx-flags = ["-Wno-suggest-destructor-override", ...]  # upstream's 7 relaxations

[targets.simplecpp]
system-includes = true
cxx-flags = ["-Wno-zero-as-null-pointer-constant"]
```

`[profiles.release] cxx-flags = ["-O2"]` **stays where it is** — it is
config-specific, and profile-over-builtin last-wins (layer 4 over layer 1)
is now documented contract. The Clang-13-only per-source relaxation on
`processexecutor.cpp` remains out of scope (per-source, deferred; hoisting
to target-private is the blessed workaround).

### 4.5 benchmark — per-target overrides (GAPS §3)

Before: 20-flag warning battery pasted into two profiles; test-only
suppressions applied globally as a documented deviation. After:

```toml
[flags]
cxx-flags = ["-Wall", "-Wextra", "-Wshadow", "-Wfloat-equal",
             "-Wold-style-cast", "-Wconversion", "-Wformat=2", "-Werror",
             "-pedantic", "-pedantic-errors", "-fstrict-aliasing",
             "-Wstrict-aliasing", "-Wthread-safety",
             "-fvisibility=hidden", "-fvisibility-inlines-hidden"]

[targets.basic_test]
cxx-flags = ["-Wno-unused-but-set-variable"]  # test-dir suppression, scoped at last

[targets.donotoptimize_test]                  # when the suite lands (B2)
cxx-flags = ["-O3", "-Werror=deprecated-declarations", "-UNDEBUG"]
```

Target-private (layer 7) after profile (layer 4) makes the `-O3` override
work by the documented rule. `-UNDEBUG` passes the lint (no schema home
for un-defines, deliberately).

### 4.6 vtz, cpptrace, googletest

- vtz: the 8-flag dev-warning set moves from two profile blocks to
  `[flags]`; `-Wall -Wextra` scoped to the two object-lib-equivalent
  targets as private `cxx-flags` (upstream scopes them to `vtz_objects`);
  `-Wshorten-64-to-32` stays annotated "clang-only — move under
  `cfg.clang` when B3 lands".
- cpptrace: `-fvisibility=hidden -fvisibility-inlines-hidden -Wall -Wextra
  -Werror=return-type -Wundef` move from `[profiles.relwithdebinfo]` (where
  they existed in exactly one profile and hit the demo target too) to
  `[targets.cpptrace] cxx-flags` private — upstream's actual scoping, in
  all profiles, off the demo.
- googletest: the four library targets get upstream's strict set as
  private `cxx-flags`; samples get the milder set; on GCC/Linux the port
  stops silently losing `-Wextra` (GAPS §7's exact complaint).

---

## 5. Interaction analysis

### 5.1 × cfg (B3)

Flags are among the five things the backlog says any cfg answer MUST cover
(sources, defines, deps, link inputs, **flags** — abseil/benchmark
evidence). Contract this design lodges with the cfg design, whichever
surface wins there:

- The three flags keys must be legal under a target-scope cfg predicate
  (`[targets.time.cfg.macos] link-flags = [...]` in the sub-table
  candidate; `{ flag = "-lrt", when = "linux" }` entries in the inline
  candidate) **and** under `[flags]` (`[flags.cfg.linux]`), with merge =
  append-after-base at the same layer (base layer 3/6/7 content first,
  then cfg refinement — so platform-specific entries win by last-wins
  within their layer).
- Visibility composes *outside* cfg: a cfg block refines the
  public/private lists, it does not introduce a third visibility. (In the
  sub-table candidate: `[targets.x.cfg.linux] cxx-flags = { public =
  [...] }` — the split nests inside the predicate, same grammar as the
  base key.)
- Profile × cfg (`[profiles.release.cfg.linux]`) is explicitly deferred —
  no wave-1 evidence needs it once `[flags]` and target flags exist.
- The abseil `time` example (§4.3) is the acceptance test: private
  link-flags + macOS predicate + static-lib link-only propagation must
  compose into "exactly Darwin builds of closures containing `time` link
  CoreFoundation".

### 5.2 × codegen (B4)

- Generated TUs compile as members of their owning target and receive that
  target's effective flags (all seven layers). No flag surface on
  `[generate.*]` — a generator that needs special compile flags for its
  *output* argues for a dedicated target, not a new knob. cppcheck's
  matchcompiler outputs replace the originals inside the same target and
  therefore inherit identically — which is what upstream's
  `add_custom_command`+same-target structure does.
- `${gen}` include roots are ordinary project `-I`, never `-isystem`
  (§0.5): generated code is your code.

### 5.3 × tests (B2)

- Test-marked targets are project targets: they get `[flags]` and profile
  layers like everything else (ninja's `-Wno-deprecated` must reach
  `ninja_test`; cppcheck's `-Weverything` reaches `testrunner` upstream
  too). What they must NOT inherit is another target's *private* policy —
  which is precisely what moving `-Werror` from profile scope to target
  scope fixes (json-tui). No test-specific flag mechanism is needed; the
  B1+B2 combination dissolves the wave-1 break by construction.
- Dev-dependencies (B2) are ordinary store deps: their headers arrive
  `-isystem` via §0.5, so a strict-flags project's tests don't inherit
  gtest's warnings. (This is the second half of the json-tui fix and it is
  tool-side, already decided.)

### 5.4 × target-defaults (B9)

`[flags]` and `[target-defaults]` are different verbs and must stay so:
`[flags]` **concatenates** (a layer every target gets, not overridable,
only appendable-after); `[target-defaults]` **fills in absent fields**
(a default the target may replace). If B9 chooses to admit flags keys in
`[target-defaults]`, its fill-if-absent semantics apply to the whole key
("as if written in the target") — this design neither requires nor
forbids that; it only claims the concatenating layer, which is what 5 of 8
projects duplicated across profiles. The two compose without ambiguity
because they occupy different layers (3 vs 6/7).

### 5.5 × install/export (B6) and interface-library (B10)

- Public flags (A/C) / knobs (B) are interface metadata: the future
  export emitter must serialize them (`INTERFACE_COMPILE_OPTIONS` /
  `INTERFACE_LINK_OPTIONS` in Config shims, the corresponding CPS fields).
  Candidate C makes that exported surface reviewable by construction (no
  warning/opt/sanitizer classes can appear in it). Extraction already
  imports the same properties from store deps; round-trip fixpoint holds.
- `interface-library` (B10) targets have no private compile step; their
  flags surface is public-only (A/C) or knob-only (B). The `interface`
  visibility bucket, when un-deferred, slots into §0.4 unchanged.
- The `frameworks` bucket that extraction already maintains for imported
  targets is deliberately NOT given a native-target twin here:
  `link-flags = ["-Wl,-framework,X"]` under a cfg predicate covers the
  corpus (one site), and a first-class `frameworks` field is macOS sugar
  that can be added later without disturbing anything in this design
  (recorded as OQ6).

### 5.6 × ABI classification and profiles (existing)

Fully specified in §0.2/§0.3: the classifier gains callers, not rules.
Profile flags' semantics are untouched; `[flags]` is hoisted-profile
semantics; target flags are strictly consumer-project-scope with ABI
entries refused. No existing store entry is invalidated by any candidate
(new layers are empty by default — charter tie-breaker 2).

---

## 6. Linux story (S5/S6, gcc 16 / clang 22 on Arch)

- **Mechanism is platform-neutral**: flags are opaque argv words routed to
  drivers; nothing in the layering, propagation, or classifier touches an
  OS API. The classifier's prefixes (`-W`, `-O`, `-g`, `-fsanitize`,
  ABI table) are GCC-shape and cover both gcc and clang; MSVC spellings
  are future additions behind B3's `compiler` predicate.
- **`-isystem` behaves identically** on GNU drivers (suppresses
  diagnostics, reorders search after `-I`); gcc-16 and clang-22 both
  honor it. The json-tui fix carries over unchanged.
- **Compiler-conditional flags become load-bearing on keres**: vtz's
  `-Wshorten-64-to-32` (clang-only; gcc errors on it at `-Werror` level
  since it's an unknown warning to gcc within `-W` handling of
  non-`-Wno-` forms) is the acceptance case for cfg × flags (§5.1). Until
  B3 lands, the documented interim is a comment + toolchain choice, as
  today.
- **Link-flag propagation order matters more on ELF**: GNU ld with
  `--as-needed` (default on Arch) is ordering-sensitive where Apple's ld
  is forgiving. §0.3's dependents-before-dependencies closure order for
  propagated link-flags matches the library-input order that already
  works, so `-Wl,...` contributions land in a safe position by
  construction. Raw `-lrt`/`-pthread` remain **link inputs, not flags**
  (B7/B3 territory); this design refuses to become the smuggling route for
  them beyond the documented-discouraged escape.
- **`-Wl,-framework,CoreFoundation`** and friends must sit under
  `cfg.macos` to keep one manifest portable — flagged in §4.3/§5.1; on
  Linux the entry simply doesn't exist, no driver error to design around.
- **PIC**: `-fPIC` via `[flags]` reaches all project targets uniformly
  (vtz GAPS §4 notes this becomes real with shared libs on Linux); it is
  not ABI-classified in v1, so it does not rebuild deps — correct, since
  store deps make their own PIC choice and mixing PIC objects into
  non-PIE/PIE links is handled by the driver.

---

## 7. Implementation sketch (all candidates; deltas noted)

- `src/schema.rs`
  - `RawTarget` += `cxx_flags`, `c_flags`, `link_flags`
    (A/C: `Option<RawVisibility>` reusing the existing untagged
    bare-or-split enum; B: `Vec<String>` + `exceptions: Option<bool>`,
    `rtti: Option<bool>`), `system_includes: Option<bool>`.
  - `RawProject` += `flags: Option<RawFlags>` (struct = the three lists;
    reuse `RawProfile`'s shape).
  - `RawDependency` += `system_includes: Option<bool>` (default true).
  - Validation pass: ABI-flag rejection at target scope (calls into the
    classifier), public-flags-on-executable error, dedicated-key lints;
    C: propagation-class fence; B: knob conflict check moves to graph
    (needs the closure).
- `src/toolchain.rs`
  - Expose the classifier (`pub fn classify_flag`) and extend with
    warning/opt-debug classes (C only). `[flags]` ABI entries join the
    existing profile-ABI injection set (same function, one more source).
    `-isystem` emission already exists (line ~264); no change.
- `src/graph.rs`
  - `PlannedTarget` += per-target `cxx_flags`/`c_flags`/`link_flags`
    (effective, post-layering). `plan()` computes: layer concatenation
    (§0.3), public-flag propagation over the existing compile-visibility
    closure with contributing-target dedup (§0.4), link-flag closure
    collection riding the existing link-only edge walk, and the
    `system-includes` consumer-side include classification (route a
    sibling target's public includes into the `-isystem` emission path).
    B: knob expansion + conflict errors here.
- `src/ninja_gen.rs`
  - Per-target `cxx_flags`/`c_flags` variables (the per-target
    `link_flags` var already exists at line ~298; compile-side today is
    profile-global — becomes a per-target override var). Mechanical.
- `src/hashing.rs`
  - Fold `[flags]` ABI-classified entries into dependency config hashes
    via the existing profile-ABI rule (one more input to the same hash
    field). Target-scope flags: no hash contribution, asserted by test.
- `src/probe.rs` / `src/manifest.rs`
  - The decided tool fix (imported interface includes → `system_includes`
    bucket) + honor the per-dep `system-includes = false` opt-out at
    manifest-ingestion time.
- `src/cli.rs`: surface the new lints through the existing `Warnings`
  channel.
- Docs: CPPKG_TOML.md gains the `[flags]` section, the target keys, the
  ordering contract (§0.3), and — independently overdue — the
  `find-package` field (B12).

Estimated size: schema+graph dominated; no store-format or lockfile
changes; no dependency rebuilds for existing projects (empty layers hash
to nothing new). C over A is ~40 lines of classifier + tests. B is the
largest graph delta (knob propagation + conflict detection).

---

## 8. Honest costs (cross-candidate)

- **A**: no fence — the public bucket is a graph-wide injection channel
  and the ecosystem will eventually use it badly; the tool will be
  standing right there, saying nothing. Cheapest today, priciest to
  retrofit opinions onto (any later fence is a breaking change for
  whoever leaned on the gap).
- **B**: covers wave 1 perfectly and wave 2 probably; then the first
  un-curated propagating flag arrives and there is no escape hatch —
  library authors are the users hit, at export time (B6), exactly where
  cpp-pkg wants to win. Vocabulary maintenance is forever; MSVC doubles
  it.
- **C**: carries a judgment table; every rejection must be defensible and
  the table must grow with driver families. Mitigations: unknown ⇒
  allowed (fails open toward A, never rejects falsely), the four v1
  classes are prefix-shaped and already half-implemented, and the errors
  are load-time with one-line explanations.
- **All**: per-source flags remain unexpressible (deliberate; the
  evidence says B4 subsumes them — if wave 2 falsifies that, this design
  does not have to move, a `sources` entry-table form does).
  `-isystem`-by-default for deps changes diagnostics (not artifacts) for
  existing migrations — strictly toward upstream parity, but it is a
  behavior change and should be release-noted with the `system-includes =
  false` opt-out.

## Recommendation

**Candidate C**, runner-up A. The grammar is A's — one primitive, the
existing visibility shape, bare-list sugar, nothing new to learn (charter:
cargo-familiar, tie-breaker 3 and 4). The fence is one table that already
half-exists, it turns every foreseeable public-flag footgun into a
one-line load-time error, and it fails open (unknown flags propagate), so
it can never strand a legitimate use the way B's closed vocabulary can.
Wave-1 evidence is fully covered by all three; C is the only one that is
both sufficient for the corpus and opinionated exactly where the sketch
identified the danger. B's knobs survive as optional sugar over C (OQ2) —
with conflict checking — rather than as the load-bearing surface.

---

## OPEN QUESTIONS for the taste judge

1. **A vs B vs C** — the S1 contested call: open public strings, curated
   knobs, or open-with-fence. (Recommendation: C; runner-up A. If C's
   classifier feels like too much opinion, A + a warning-only version of
   the same table is the halfway house.)
2. **Knob sugar on top of C** (`exceptions = false`, `rtti = false` ≡
   checked public `-fno-*`): ship in v1, or defer until a second corpus
   site wants it? (json-tui is the only wave-1 site; it reads better with
   the knob but needs only the flag.)
3. **Lint severity** for `-D*`/`-I*`/`-std=*` inside flags lists: warning
   (recommended — migrations paste flag soup, `-UNDEBUG` must stay legal)
   or hard error (tie-breaker 1 purism)?
4. **`system-includes = true` on project targets** (vendored-code case,
   cppcheck): land with B1 (recommended — it is one boolean and completes
   the warnings-policy story) or park it with B6/B10 where header
   exporting lives?
5. **Per-dependency `system-includes = false` opt-out**: worth the knob in
   v1, or YAGNI until someone asks to see dep warnings? (Zero corpus
   demand; it exists here for CMake parity and cheap symmetry.)
6. **`frameworks = [...]` as a first-class native-target field** (abseil
   GAPS suggests it; extraction already has the bucket for imported
   targets) vs. staying with `link-flags = ["-Wl,-framework,X"]` under
   `cfg.macos` (recommended: defer — one corpus site, pure sugar, purely
   additive later).
7. **`[flags]` naming**: confirm `[flags]` over `[build]`/`[profiles.all]`
   (rejections argued in §0.1), and confirm it hosts all three keys
   symmetric with profiles.
8. **Public link-flags ≡ private on static libraries** (§0.2): accept the
   documented equivalence now with the split reserved for a future
   shared-library kind, or reject the public bucket for `link-flags`
   entirely until it means something? (Recommendation: accept + document;
   rejecting creates a gratuitous asymmetry with `cxx-flags`.)
