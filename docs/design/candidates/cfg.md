# cfg — B3 platform conditionals, layer 1 (candidate designs)

Area: **cfg**. Scope: static predicates (`os`, compiler family) selecting
**sources, defines, dependencies, link inputs, and flags** in the project
manifest. Layer 2 (probes: has-symbol, try-compile, has-include) is
**documented out of scope** and routed to a future probe design; this file
defines the boundary and the blessed interim pattern for transcribed probe
answers.

> **NEAR-IRREVERSIBLE.** The cfg surface is the single most-imitated shape in
> a manifest language: once real manifests exist in the wild, every list key
> in the schema carries this grammar forever. cxx-flags can grow a field;
> cfg *is* the grammar of half the file. The A/B/C choice below is a one-way
> door — pick with the wave-2 corpus (Arrow: "too large to hand-re-derive per
> platform") in mind, not just wave 1.

Evidence base: BACKLOG.md B3 + S3; GAPS.md of ninja (§1, §4), abseil
(conditional-sources, dep-provisioning §2–3), benchmark (§2, §4, §7),
cpptrace (§5), cppcheck (§5–6), vtz (§4–5), googletest (§7), json-tui.
8/8 projects: every wave-1 manifest is a macOS projection.

---

## 0. Common semantics (identical across all three candidates)

Everything in this section is candidate-independent. The candidates differ
only in *where the predicate lives in the TOML*; the predicate model, merge
model, lockfile behavior, and error behavior below are shared, so the taste
decision is purely about surface.

### 0.1 Predicate vocabulary — closed, tiny, v1

Two axes, both answered by the **toolchain identity**, never by ad-hoc host
sniffing:

| Axis | Atoms (v1) | Truth source |
|---|---|---|
| os | `windows`, `macos`, `linux` | parsed from `ToolchainIdentity.target_triple` (`*-windows-*`/`-msvc`, `*-apple-darwin*`, `*-linux-*`) |
| os family | `unix` | true iff os ∈ {macos, linux} (future unixes join the family additively) |
| compiler | `clang`, `gcc`, `msvc` | mapped from `ToolchainIdentity.compiler_id`: `AppleClang`→clang, `Clang`→clang, `GNU`→gcc, (future) `MSVC`→msvc |

Decisions folded in:

- **`clang` matches AppleClang.** googletest GAPS §7 is direct evidence that
  distinguishing them by default reproduces CMake's `STREQUAL "Clang"`
  footgun (upstream's own warnings silently vanished on AppleClang). A
  narrower `apple-clang` atom is *reserved*, not in v1 (no wave-1 site needs
  the distinction).
- **Truth comes from the target triple**, not the build host. v0 has no
  cross-compilation so host == target today, but the semantics is stated now
  so cross-compilation later changes nothing: cfg answers "what am I building
  *for*", the only question a manifest can meaningfully ask.
- **The vocabulary is closed.** An unrecognized atom is a **hard error at
  manifest load** listing the vocabulary — never a silently-false predicate.
  This is what makes cfg greppable and typo-safe. New atoms (`freebsd`,
  `apple-clang`, `mingw`…) are additive, gated on a migration that needs
  them.
- **Combinators are reserved, not implemented.** `all(...)`, `any(...)`,
  `not(...)` (and compiler-version comparisons) parse to a *distinct* error
  — "reserved, not available in v1" — so adding them later is non-breaking.
  Zero wave-1 sites need a conjunction: every instance is a single os or a
  single compiler test. (`not(windows)` is spelled `unix` today.)
- **Out of vocabulary, stated honestly:** benchmark's Solaris `kstat`,
  ninja's AIX `-lperfstat`/getopt-as-C++, cppcheck's musl-vs-glibc
  `execinfo.h`, Cygwin `_GNU_SOURCE`. These remain undeclarable in v1;
  manifests document them in comments exactly as today. This is a deliberate
  cost (see §7), not an oversight: cpp-pkg can only honestly ship predicates
  it can CI.

### 0.2 Evaluation model

- Predicates are evaluated **at plan time** (graph::plan), after toolchain
  detection, before glob expansion. `build.ninja` is regenerated every build
  in v0, so switching `--toolchain` re-evaluates cfg with zero staleness.
- Multiple predicates may be simultaneously true (`linux` and `unix` and
  `gcc`). **All matching conditional groups apply.** Non-matching groups are
  parsed and validated (predicate vocabulary, key restrictions) but their
  globs are never expanded and their paths never checked — a macOS checkout
  need not contain the files a `windows` block names, and a `windows` glob
  matching zero files on a Windows checkout follows the existing empty-glob
  behavior there.
- **Merge is additive-only, append semantics.** Conditional content appends
  to the unconditional content of the same key: sources append to sources,
  `defines.private` to `defines.private`, flags to flags (so platform flags
  land *after* base flags — consistent with the documented last-wins flag
  contract). Order: unconditional entries first, then matching conditional
  groups in document order.
- **Only list-valued keys are conditionable in v1**: `sources`, `includes`,
  `defines`, `dependencies` (target-scope), and — once B1 lands —
  `cxx-flags`, `c-flags`, `link-flags`. Scalar keys (`type`, `cxx-std`,
  `c-std`) inside a conditional group are a **hard error** ("conditional
  scalar overrides are not in v1"). Rationale: append semantics is trivially
  order-insensitive and confluent; scalar override semantics (who wins when
  `linux` and `unix` both set `cxx-std`?) is a swamp we do not need — no
  wave-1 site conditions a scalar. (vtz's STATIC/SHARED switch is an
  *options/feature* question, explicitly not cfg.)

### 0.3 Conditional dependencies, the lockfile, and the store

- **All declared dependencies are resolved and locked, always** — including
  ones behind a currently-false predicate. `CppPkg.lock` is therefore
  platform-independent and committable (Cargo semantics: the lockfile pins
  every cfg branch). Cost accepted: locking a windows-only dep from a Mac
  needs one `ls-remote`-class network round trip at resolve time.
- Only dependencies whose predicate is true are **fetched, built, probed,
  and admitted to the target-reference namespace** on this build. A
  `dependencies` reference from an *active* target to a cfg'd-out package's
  target is an ordinary unresolved-reference error, augmented with "declared
  behind cfg `<pred>`, which is false for this toolchain".
- A `needs` edge from an active dep to a cfg'd-out dep is a **resolve-time
  error** naming the predicate (the declared graph must be self-consistent
  per evaluated platform).
- **No new config-hash inputs.** Project targets are not store entries, and
  a dependency's own build is unaffected by which predicate selected it.
  Crucially, **extracted dependency manifests need no cfg at all**: the
  probe runs `find_package` on the target platform and store entries are
  already keyed by toolchain identity, so dep-side platform variation is
  handled by per-platform re-extraction. (The B12 fix — actually evaluating
  `$<BOOL:...>` genexprs in LINK_ONLY instead of skipping them — is the
  *tool* half of that story and is required for Linux regardless of this
  design; abseil's `-lrt` is silently dropped without it.) **cfg is a
  project-manifest concern only.**

### 0.4 Error behavior (summary)

| Condition | Behavior |
|---|---|
| unknown predicate atom | hard error at load, lists vocabulary |
| combinator / version predicate | hard error: "reserved, not in v1" |
| scalar key in a conditional group | hard error |
| nested conditional inside a conditional | hard error (no predicate stacking in v1) |
| empty conditional group | lint warning (allowed — generators emit them) |
| dep key declared both unconditionally and conditionally, or in two conditional groups | hard error in v1: a dep key is declared in exactly one place (per-OS *pins* of the same key have no wave-1 evidence; revisit on evidence) |
| active target references cfg'd-out dep target | resolve error naming the false predicate |
| `needs` reaches a cfg'd-out dep | resolve error naming the false predicate |
| target has zero sources after evaluation | existing no-sources error, message extended to mention which cfg groups did not match |

### 0.5 The layer-2 boundary, and the blessed interim

Layer 2 — `has-symbol(ppoll)`, `try-compile`, `has-include(execinfo.h)` —
is **out of scope** until a probe design exists (it overlaps B4; benchmark
GAPS calls it "the moral equivalent of build.rs"). Layer 1 deliberately
leaves those manifests *partially* wrong on unprobed axes, but changes the
failure mode:

**Blessed interim pattern — labeled transcription.** A probe answer is
transcribed under the *narrowest true predicate*, with a comment naming the
upstream check it transcribes:

- ninja: `USE_PPOLL` under `linux` (`# transcribed: check_cxx_symbol_exists(ppoll)`)
- benchmark: the `HAVE_*` sets under `macos` and `linux` separately
- cppcheck: `HAVE_EXECINFO_H=1` under `macos`+glibc-`linux` reality —
  transcribed under `linux` *knowingly wrong on musl*, and the comment says
  so.

This is strictly better than today on all three axes the charter cares
about: the declarative reading stops lying ("on linux, define X" is exactly
what happens), a wrong answer is *labeled* with the platform it was derived
on, and the eventual layer-2 design has a mechanical migration target (each
transcribed define becomes a probe result). All predicate grammars below
keep the quoted-key / string space open so probe predicates
(`"has-symbol(ppoll)"`) can slot into the same positions later without a
new mechanism.

---

## 1. Candidate A — `cfg` sub-tables (block-scoped merge)

The BACKLOG S3 sketch, refined. One orthogonal rule: **a conditionable
scope may contain a `cfg.<predicate>` sub-table whose contents merge
additively into that scope.** Two scopes exist in v1:

1. **Target scope** — `[targets.<t>.cfg.<pred>]`, containing any of the
   target's list-valued keys.
2. **Package scope** — `[cfg.<pred>]`, containing `dependencies` (and
   `flags` once B1's profile-independent flag block lands; `dev-dependencies`
   once B2 lands). Cargo users will recognize
   `[target.'cfg(windows)'.dependencies]` — this is that, minus the quoting
   ceremony.

Refinement over the sketch: the sketch's `[dependencies.zlib.cfg.linux]`
(cfg *inside* a dep) is dropped. cfg inside a dep would condition dep
*fields* (per-OS `options`?) — no wave-1 evidence, and it muddles "is this
dep present" with "what does this dep look like". Presence-conditioning
lives at package scope: `[cfg.windows.dependencies.winreg]`. Per-field dep
cfg stays reserved.

Reserved (error today, additive later, same rule): `[cfg.<pred>.targets.*]`
(whole conditional targets), `[cfg.<pred>.generate.*]` (B4 steps — ninja's
browse mode is genuinely posix-only upstream), `[target-defaults.cfg.<pred>]`
(B9). The single rule scales to all of them without new grammar.

### 1.1 Grammar

```toml
[targets.<name>.cfg.<atom>]
# any subset of: sources, includes, defines, dependencies,
#                cxx-flags, c-flags, link-flags (post-B1)
# same shapes as in the target proper (visibility splits, bare-list sugar)

[cfg.<atom>.dependencies.<key>]
# a full, ordinary dependency table
```

`<atom>` is a bare key from §0.1. Future combinators arrive as quoted keys
(`[targets.x.cfg."all(linux, gcc)"]`) — reserved.

### 1.2 Corpus sites, before → after

**ninja `libninja` (the canonical case).** Before: 27 + 2 posix sources
hard-wired, comment "not portable to Windows" (migrations/ninja/CppPkg.toml
lines 51–94). After:

```toml
[targets.libninja]
type = "static-library"
cxx-std = 11
sources = [
    "src/build_log.cc", "src/build.cc", # ... 27 platform-neutral files
]

[targets.libninja.cfg.unix]
sources = ["src/jobserver-posix.cc", "src/subprocess-posix.cc"]

[targets.libninja.cfg.linux]
# transcribed: check_cxx_symbol_exists(ppoll ...) — true on Linux, false on
# macOS. Layer 2 owns the real probe; without this line a Linux build
# silently falls back to pselect (BACKLOG B3).
defines = { private = ["USE_PPOLL"] }

[targets.libninja.cfg.windows]
sources = [
    "src/subprocess-win32.cc", "src/includes_normalize-win32.cc",
    "src/jobserver-win32.cc", "src/msvc_helper-win32.cc",
    "src/msvc_helper_main-win32.cc", "src/minidump-win32.cc",
    "src/getopt.c",
]
defines = { private = ["NOMINMAX"] }

[targets.ninja_test.cfg.windows]
sources = ["src/includes_normalize_test.cc", "src/msvc_helper_test.cc"]
```

(Out of scope, unchanged: AIX getopt-compiled-as-C++ — extension-table
question; `windows/ninja.manifest` on exes — Windows toolchain work; MSVC
flag set expressible under `cfg.msvc` the day a Windows toolchain exists.)

**abseil `absl::time` CoreFoundation (link flags, the "flags and link
inputs, not just sources" proof).** Before: hoisted to
`[profiles.release] link-flags` — every executable links CoreFoundation,
and `--config debug` silently loses it (migrations/abseil/header.toml).
After (with B1 per-target `link-flags`):

```toml
[targets.time.cfg.macos]
# upstream: $<$<PLATFORM_ID:Darwin,iOS,tvOS,watchOS>:-Wl,-framework,CoreFoundation>
link-flags = ["-Wl,-framework,CoreFoundation"]

[targets.base.cfg.linux]
link-flags = ["-lrt"]          # upstream: $<$<BOOL:${LIBRT}>:-lrt>

[targets.synchronization.cfg.linux]
link-flags = ["-pthread"]      # via Threads::Threads; moves to B7's
                               # system-dep form when that lands — see §6.5
```

Three-line diff to `gen_toml.py`'s LINKOPTS handling; the generated file
stays reviewable.

**benchmark (probe transcription + per-OS link inputs).** Before: macOS
`HAVE_*` answers as unconditional private defines, `-lrt`/`shlwapi`/`kstat`
undeclarable (migrations/benchmark/CppPkg.toml lines 86–114). After:

```toml
[targets.benchmark.cfg.unix]
defines = { private = ["_FILE_OFFSET_BITS=64", "_LARGEFILE64_SOURCE",
                       "_LARGEFILE_SOURCE"] }   # upstream's non-MSVC branch

[targets.benchmark.cfg.macos]
# transcribed: cxx_feature_check results, macOS/arm64/AppleClang 21
defines = { private = ["HAVE_STD_REGEX", "HAVE_STEADY_CLOCK",
                       "HAVE_THREAD_SAFETY_ATTRIBUTES"] }

[targets.benchmark.cfg.linux]
# transcribed: same checks re-run on Arch/gcc16 during Linux bring-up
defines = { private = ["HAVE_STD_REGEX", "HAVE_STEADY_CLOCK",
                       "HAVE_THREAD_SAFETY_ATTRIBUTES",
                       "HAVE_POSIX_REGEX", "HAVE_PTHREAD_AFFINITY"] }
link-flags = ["-lrt"]          # transcribed: check_library_exists(rt shm_open)

[targets.benchmark.cfg.windows]
link-flags = ["shlwapi.lib"]
```

Solaris `kstat` remains undeclarable (out-of-vocabulary, §0.1) — the
manifest keeps a comment, as today.

**vtz clang-only warning (compiler axis).** Before: `-Wshorten-64-to-32`
pasted into every profile, wrong under `--toolchain gcc-homebrew` (vtz GAPS
§4). After (with B1): `[targets.vtz.cfg.clang] cxx-flags =
["-Wshorten-64-to-32"]`. googletest's Clang-gated `-W` battery: identical
shape.

**cpptrace / cppcheck (define decision trees).** cpptrace's ~8 autoconfig
answers split into `[targets.cpptrace.cfg.macos.defines]` /
`[targets.cpptrace.cfg.linux.defines]` blocks with the derivation comments
it already carries; cppcheck's `HAVE_EXECINFO_H=1` moves under `cfg.macos`
+ `cfg.linux` with the musl caveat in a comment. Pure labeled transcription
(§0.5) — the *questions* stay layer 2.

### 1.3 Assessment

For: the corpus's dominant shape is **platform blocks**, not single entries
(ninja: 7 sources + a define + a flag set per platform; benchmark: define
sets; cpptrace: define sets) — A expresses a block as one table with one
predicate stated once. One orthogonal rule covers targets, deps, flags,
and (reserved) defaults/generate/targets — maximal composition with
B1/B9/B2 for free. Greppable (`grep -rn 'cfg\.linux'` finds every Linux
delta in a tree). Cargo-familiar. No new entry grammars: the contents of a
cfg block are *exactly* the existing key shapes.

Against: one-entry deltas cost a table header (abseil CoreFoundation: 2
lines vs B's 1) and separate the delta from its base list — a reader of
`sources` must scan for cfg blocks below (mitigated: `cfg` tables must
follow the keys they modify by convention, and the no-scalar rule means a
cfg block can never change the *meaning* of what was read above, only
append). Appended-after ordering means a cfg flag cannot be interleaved
*before* a base flag (no wave-1 need; last-wins makes append the useful
direction).

---

## 2. Candidate B — inline `when` entries

One orthogonal word: **`when = "<atom>"` may appear on any list entry (in
its table form) and on any gateable table.** Variation lives inside the
list, next to what varies. This grows the string-or-table entry precedent
the schema *already reserves* for `dependencies` ("table form
`{ target = "...", ... }` reserved for per-edge attributes").

### 2.1 Grammar

Entry-table forms (string entries remain sugar for the unconditional case):

| List | Entry table |
|---|---|
| `sources` | `{ path = "src/x.cc", when = "windows" }` (path may be a glob) |
| `includes.{public,private}` | `{ path = "include-win", when = "windows" }` |
| `defines.{public,private}` | `{ define = "NOMINMAX", when = "windows" }` |
| `dependencies.{public,private}` | `{ target = "winreg::winreg", when = "windows" }` — extends the reserved form |
| `cxx-flags` / `link-flags` (post-B1) | `{ flag = "-lrt", when = "linux" }` |

Gateable tables: `[dependencies.<key>] when = "windows"` (presence gate);
later `[generate.<name>] when = "unix"` (B4), `test = ...` run entries (B2).
Evaluation of an entry: predicate false ⇒ the entry does not exist (its
glob unexpanded, per §0.2). Everything else — vocabulary, additivity (an
entry is inherently additive), lockfile, errors — per §0.

### 2.2 Corpus sites, before → after

**ninja `libninja`:**

```toml
[targets.libninja]
type = "static-library"
cxx-std = 11
sources = [
    "src/build_log.cc", # ... 27 platform-neutral files
    { path = "src/jobserver-posix.cc",           when = "unix" },
    { path = "src/subprocess-posix.cc",          when = "unix" },
    { path = "src/subprocess-win32.cc",          when = "windows" },
    { path = "src/includes_normalize-win32.cc",  when = "windows" },
    { path = "src/jobserver-win32.cc",           when = "windows" },
    { path = "src/msvc_helper-win32.cc",         when = "windows" },
    { path = "src/msvc_helper_main-win32.cc",    when = "windows" },
    { path = "src/minidump-win32.cc",            when = "windows" },
    { path = "src/getopt.c",                     when = "windows" },
]
defines = { private = [
    { define = "NOMINMAX", when = "windows" },
    # transcribed: check_cxx_symbol_exists(ppoll) — layer 2 owns the probe
    { define = "USE_PPOLL", when = "linux" },
] }
```

One annotated list — the win32/posix split reads top-to-bottom exactly like
upstream's file layout. Cost on display: `when = "windows"` nine times, and
the `defines` visibility split now nests tables inside tables.

**abseil `absl::time`:** the best case for B — generator-emitted
one-liners:

```toml
[targets.time]
link-flags = [{ flag = "-Wl,-framework,CoreFoundation", when = "macos" }]
[targets.base]
link-flags = [{ flag = "-lrt", when = "linux" }]
```

**benchmark:** the worst case for B — the macOS/linux define *sets* become
eight `{ define = ..., when = ... }` tables interleaved in one
`defines.private` array (or grouped by convention), each repeating its
predicate; `link-flags = [{ flag = "-lrt", when = "linux" },
{ flag = "shlwapi.lib", when = "windows" }]` is fine.

**vtz/googletest compiler-gated flags:**
`cxx-flags = ["-Wall", "-Wextra", { flag = "-Wshorten-64-to-32", when = "clang" }]`
— B's most attractive single line in the whole corpus.

**Conditional dep presence:** `[dependencies.winreg] when = "windows"` plus
the usual source fields.

### 2.3 Assessment

For: variation is adjacent to what varies — no scanning below the target
for blocks; single-entry deltas are one-liners; ordering is manifest-literal
(a conditional flag can sit anywhere in the flag order, not just appended);
extends a grammar position the schema explicitly reserved; test/generate
gating later reuses the same word.

Against: **five new entry micro-grammars** (`path` / `define` / `target` /
`flag`, each a tiny schema with its own error messages) versus A's zero;
block-shaped variation — the corpus's dominant shape — degenerates into
per-entry predicate repetition (ninja ×9, benchmark ×8); mixing bare
strings and inline tables in one array is the least "minimal" reading in
the current grammar and the annotated `defines` array (tables inside a
visibility-split table) is genuinely hard on the eyes; TOML forbids
multi-line inline tables, so any entry gaining a second attribute later
(per-edge `whole-archive` + `when`) starts fighting line length.
Greppability is weaker in practice: `when = "linux"` finds the entries, but
reconstructing "what is the Linux delta of this target" means reading every
list.

---

## 3. Candidate C — predicate-keyed value tables (developed, then argued against)

The remaining shape the sketch did not consider: **a list-valued key may be
a table from predicate atoms to lists**, with `all` as the unconditional
bucket and a bare array as sugar for `{ all = [...] }` (matching the
existing bare-list sugar convention).

### 3.1 Grammar and corpus sites

```toml
[targets.libninja]
type = "static-library"
cxx-std = 11

[targets.libninja.sources]
all = ["src/build_log.cc", ...]            # 27 files
unix = ["src/jobserver-posix.cc", "src/subprocess-posix.cc"]
windows = ["src/subprocess-win32.cc", ...] # 7 files
```

Visibility-split keys nest visibility **outside**, predicate **inside**
(the only order that keeps `defines = { private = [...] }` sugar working):

```toml
[targets.libninja.defines.private]
all = []
windows = ["NOMINMAX"]
linux = ["USE_PPOLL"]

[targets.time.link-flags]        # post-B1
macos = ["-Wl,-framework,CoreFoundation"]
```

Reads like a match statement; benchmark's define sets are as clean as A;
abseil's one-liner is one line.

### 3.2 Why it loses (argued honestly, not dismissed)

- **Two keyed-table conventions collide on the same keys.** `sources` is
  predicate-keyed at the top level; `defines` is visibility-keyed at the top
  level and predicate-keyed one level down. The nesting order is
  *inconsistent across sibling keys*, and whichever order is chosen, the
  other reads as a bug. `{ public = [...], windows = [...] }` mixing must
  be a hard error — a whole error class neither A nor B has.
- **Namespace fusion.** Predicate atoms (`windows`, `linux`, …) and bucket
  words (`all`, `public`, `private`) share one key namespace forever; every
  future atom and every future bucket must dodge each other. `all` as a
  bucket also permanently shadows `all(...)` the reserved combinator.
- **It conditions values, not scopes.** Dep presence, generate steps, and
  whole targets are unit-shaped, not list-shaped — C needs a second
  mechanism for them (A's package-scope rule or B's `when`), violating
  "one orthogonal primitive" by construction.

Verdict: C is A's block ergonomics bought with a permanently confusing
grammar. Rejected here; recorded so the taste judge sees it was developed,
not skipped.

---

## 4. Interaction analysis (flags × cfg × codegen × tests × defaults × system deps)

- **B1 flags (S1).** cfg must condition flags (abseil, vtz, googletest,
  benchmark — 4/8 need it on day one), so cfg and B1 should land in the
  same schema release or cfg ships with a hole. Shape coupling: A and B
  both compose with S1-A/C (list-shaped flags: A wraps them, B annotates
  entries). **S1-B's curated scalar knobs (`exceptions = false`) are
  scalars and therefore not conditionable under the v1 no-scalar rule** —
  if S1-B wins, cfg-conditioned knobs need an explicit exception or wait.
  This is a real cross-area constraint the taste judge should resolve
  jointly. The package-level `[flags]` block conditions via
  `[cfg.linux.flags]` (A) / entry `when` (B).
- **B4 codegen.** Generate steps are unit-shaped; gating is *reserved* in
  v1 (A: `[cfg.<pred>.generate.<name>]`; B: `when` on the step). Wave-1
  evidence exists (ninja's browse is posix-only upstream) but the Linux
  campaign doesn't need it (browse works on Linux). The `${gen}` include
  root and cfg compose with no interaction: a cfg block may add
  `includes = { private = ["${gen}"] }` like any other include.
- **B2 tests.** Test targets take cfg sources exactly like any target
  (ninja_test's two win32-only TUs — real wave-1 site). `dev-dependencies`
  conditions like `dependencies` (A: `[cfg.<pred>.dev-dependencies]`).
  Run-config (args/env/cwd) conditioning is **out of v1** — no wave-1
  evidence; vtz's 774 cases run with platform-independent invocations.
- **B9 target-defaults.** A's rule extends verbatim:
  `[target-defaults.cfg.windows]` (reserved until B9 lands). B annotates
  entries inside defaults lists. Merge order must be pinned when B9 lands:
  defaults-unconditional → defaults-cfg → target-unconditional → target-cfg
  (defaults never override, consistent with append-only).
- **B7 system deps.** Today `-lrt`/`-pthread` ride cfg'd link-flags — the
  honest v1 spelling. When B7's system-dependency form lands, those lines
  migrate to `[cfg.linux.dependencies] rt = { system = true }`-class
  declarations; cfg's dependency conditioning is *already the right shape*
  for it, so B7 adds no cfg surface. Threads dedup (B7-1) is orthogonal.
- **B12 genexpr evaluation.** Complementary tool fix, not cfg surface: the
  probe must evaluate `$<BOOL:...>` inside LINK_ONLY so *extracted* deps
  are correct per platform (§0.3). Without it, cfg makes project manifests
  Linux-correct while dependency manifests silently drop `-lrt` — B12 must
  land before or with Linux bring-up.
- **B6 export.** When the shim emitter points at the project's own
  manifest, it emits the *evaluated* view for the built platform — same as
  what CMake's install(EXPORT) does with resolved genexprs. Cross-platform
  config files (CMake's `$<PLATFORM_ID:...>` in exports) are a B6 question;
  cfg carries the information needed either way.
- **B8 glob exclusion.** Orthogonal; ninja GAPS explicitly notes exclusion
  cannot express the win32 split (not lexically clean). Negative patterns
  compose inside cfg blocks/entries like any glob.

## 5. Linux story (explicit — next campaign stage)

cfg layer 1 **is** the Linux enabler for project manifests; the campaign
acceptance test is: *one committed manifest, macOS + Arch (gcc 16, clang
22), both green, no edits between them.*

1. Toolchain: `toolchain.rs` already detects `compiler_id`/`target_triple`
   on GNU-dialect compilers; os mapping (§0.1) is a triple parse
   (`x86_64-pc-linux-gnu` → linux). gcc 16 → `gcc`, clang 22 → `clang`.
   No new detection machinery.
2. Manifest deltas that make wave-1 ports Linux-true: ninja `USE_PPOLL`
   under `cfg.linux` (fixes the silent pselect fallback BACKLOG names);
   abseil `-lrt`/`-pthread` under `cfg.linux` (fixes the silent miss);
   benchmark's Linux `HAVE_*` set + `-lrt` transcribed during bring-up;
   cpptrace's Linux define set (`CPPTRACE_UNWIND_WITH_UNWIND`,
   `HAS_DL_FIND_OBJECT`, …) transcribed from its autoconfig; cppcheck's
   `HAVE_EXECINFO_H` labeled. Each transcription is done once, on the Linux
   machine, against the reference CMake configure — the same parity
   protocol wave 1 used.
3. Dependency side needs **no cfg**: store entries are per-toolchain and
   re-extracted on Linux (§0.3), gated only on the B12 `$<BOOL>` fix.
4. `windows` atoms are accepted-but-false on both campaign platforms: the
   vocabulary ships Windows spellings now so ninja's manifest is complete,
   while no Windows toolchain exists (Dialect::Msvc deferred) — cfg is
   deliberately *ahead* of toolchain support there, and that is fine
   because false branches are validated but never expanded.

## 6. Implementation sketch (src/ modules)

Common (all candidates):

- `toolchain.rs`: `enum Os { Windows, Macos, Linux }`,
  `enum CompilerFamily { Clang, Gcc, Msvc }`; derive both on
  `ToolchainIdentity` from `target_triple`/`compiler_id` (~40 lines + tests
  with canned triples).
- `schema.rs`: `enum Predicate { Os(Os), Unix, Compiler(CompilerFamily) }`
  + parser with the closed-vocabulary/reserved-combinator errors (~60
  lines).
- `graph.rs::plan()`: takes the toolchain identity (already available to
  callers in `cmake_build.rs`/`cli.rs`); evaluate-and-merge pass runs
  before glob expansion (~80 lines).
- `cli.rs`/`fetch.rs`: filter to active deps for fetch/build/probe;
  `lockfile.rs`: resolve/lock all declared deps including inactive (~40
  lines). `cpp-pkg build --query` gains a one-line "cfg: os=linux
  compiler=gcc" header so users can see the evaluated view.
- Tests: `tests/` two-platform fixture — same manifest planned under a
  fake darwin triple and a fake linux triple, asserting source/define/flag
  deltas.

Candidate-specific:

- **A**: `schema.rs` — `TargetSpec.cfg: Vec<(Predicate, TargetCfgBlock)>`
  (document order preserved) where `TargetCfgBlock` reuses the existing
  field types; `ProjectFile.cfg: Vec<(Predicate, PackageCfgBlock)>` with
  `dependencies` only. One table-walking parser addition. Smallest diff of
  the three (~250–350 new lines total).
- **B**: `schema.rs` — every list field becomes `Vec<Entry<T>>` with
  `Entry { value, when: Option<Predicate> }`; five entry-table
  deserializers + their error paths; `DependencySpec.when`. Touches every
  field type (~400–550 lines), and `graph.rs` merge is trivially "filter".
- **C**: array-or-predicate-table deserializer on every list field plus the
  visibility/predicate nesting validation and its mixing errors — the
  largest and fiddliest parser surface.

## 7. Costs (honest)

- **Closed vocabulary excludes real corpus branches**: Solaris (kstat), AIX
  (perfstat, getopt-as-C++), Cygwin (`_GNU_SOURCE`), musl (`execinfo.h` is
  a libc question the os axis cannot ask). These stay comment-documented
  projections. Accepted: shipping only CI-able atoms is what keeps the
  vocabulary meaningful; each is additive later.
- **Layer 1 will be used to smuggle layer 2.** The labeled-transcription
  pattern (§0.5) *invites* users to write probe answers under os keys, and
  some will be wrong (musl). This is a deliberate trade: labeled-wrong with
  a migration path beats today's silently-wrong, but it is a real risk that
  transcriptions fossilize and layer 2 arrives to find cfg.linux blocks
  that are actually glibc blocks.
- **Manifests grow branches nobody at the keyboard can test.** A macOS
  author writing `cfg.windows` is coding blind; validation stops at
  vocabulary and shape (globs in false branches are unexpanded by design).
  Nothing short of CI on the other platform catches a wrong branch.
- **Conditional deps put unreachable pins in the lockfile** (windows-only
  deps locked from a Mac) — correct and Cargo-proven, but resolve now
  requires network for platforms you never build.
- **No scalars, no combinators, no whole-unit gating in v1** means a known
  second design round (probe predicates + combinators + generate/target
  gating) with pressure to keep the grammar coherent — the reservations in
  §0.1/§1 are the pre-commitment that round needs.
- **A-specific**: one-entry verbosity; append-only ordering. **B-specific**:
  grammar proliferation; block-shape noise; inline-table line-length
  ceiling. **C-specific**: §3.2.

## 8. Recommendation

**Candidate A**, with B recorded as runner-up. The corpus's variation is
block-shaped in 5 of 6 primary sites (ninja sources+defines, benchmark
defines+links, cpptrace defines, cppcheck defines, abseil per-target links
being the one entry-shaped exception — and that one is generator-emitted, so
its author is a script that does not care). A adds *zero* new value
grammars — a cfg block's contents are the schema you already know — and its
one rule extends mechanically to every future conditionable scope
(dev-dependencies, generate, target-defaults, flags), which is exactly
charter tie-breaker (3). It is also the cargo-recognizable shape
(tie-breaker: familiarity). B's genuinely better one-liners do not outweigh
five entry micro-grammars plus the worst rendering of the *canonical* case
(ninja). Whether A should ever grow B's `when` as sugar is left to the
taste judge (Open Question 2) — my inclination is no: two spellings of the
same conditional is how manifest dialects fork.

---

## OPEN QUESTIONS (for the taste judge)

1. **The one-way door: A, B, or C?** (§1–3. Everything in §0 is shared;
   this is purely the surface pick, and it is near-irreversible.)
2. If A: is B's inline `when` **forever rejected**, or reserved as future
   sugar for one-entry deltas? (Two spellings risk dialect split; the dep
   entry-table `when` slot exists either way because that table form is
   already reserved.)
3. `clang` matching AppleClang (§0.1): confirm, and is `apple-clang`
   reserved-only or worth shipping dormant in v1?
4. Keep the `unix` family atom, or force explicit `macos` + `linux`
   enumeration until a third unix exists? (ninja reads better with `unix`;
   explicit enumeration can never be wrong about a future BSD.)
5. Scalars in cfg scope: confirm **forever-error**, or hold the door open
   for override semantics (and if so, who wins when `linux` and `unix` both
   match)?
6. Confirm the reserved spellings for whole-unit gating
   (`[cfg.<pred>.targets.*]`, `[cfg.<pred>.generate.*]`,
   `[target-defaults.cfg.<pred>]`) so B4/B9/B2 designers write against
   them.
7. Duplicate dep key across mutually exclusive cfg branches (per-OS pins of
   the same dependency): confirm v1 hard-error, revisit-on-evidence.
8. Future combinator spelling: quoted-key `"all(linux, gcc)"` (A) /
   `when = "all(linux, gcc)"` (B) — bless now as the reserved grammar, or
   leave the reservation shapeless?
9. Cross-area: if S1 lands Candidate B (curated scalar knobs), do knobs get
   a cfg exception, or does that push S1 toward list-shaped flags? (Decide
   jointly with the flags area — §4 first bullet.)
10. Should the labeled-transcription convention (§0.5) be normative in
    CPPKG_TOML.md (a documented comment convention, lintable someday), or
    stay informal GAPS lore?
