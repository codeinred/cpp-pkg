# Design candidates — patches-sysdeps (B5 + B7)

Area: **B5** dependency `patches` field + **B7** system-dependency
declaration, well-known pseudo-package (Threads) dedup, hermeticity scan.
One design area because every piece changes **what enters a config hash**.

Evidence: BACKLOG.md B5/B7 + S5; abseil GAPS (dep-provisioning §1–3);
vtz GAPS §6b/§6c/§6d; cpptrace GAPS §2; cppcheck GAPS §6; json-tui GAPS §11;
ninja GAPS (Threads note); benchmark GAPS §7; googletest GAPS (Threads note).
Schema baseline: CPPKG_TOML.md (v0). Hash baseline: `src/hashing.rs`
(`ConfigHashInputs`, encoding `cppkg-config-hash-v1`).

## Decided tool fixes (referenced, not redesigned)

Two fixes are already decided and are **prerequisites** shipped with
whatever wins here; they remove two of the three wave-1 patch sites:

1. **Self-link edges are no-ops.** `absl::strings → absl::strings` in an
   extracted (or native) link interface is deduped, matching CMake's own
   semantics, instead of erroring as a cycle. Dissolves the abseil
   20260526.0 blocker with zero user-visible surface.
2. **Non-compilable extensions in extracted INTERFACE_SOURCES are
   skipped** (CMake's is-compilable classification). Project sources keep
   the strict extension table. Dissolves vtz's date v3.0.4 patch.

After both fixes, exactly one wave-1 case still requires a patch: vtz's
absl 20260107.1 (missing `TESTONLY` → installed Config references a
never-installed target → `find_package(absl)` hard-fails while *loading*).
No tool fix can save a Config that errors during load. That is the shape
the `patches` field exists for, and per vtz GAPS the install-then-probe
pipeline is stricter than the CPM ecosystem — it **will** keep finding
these. The field is the escape hatch; the fixes keep it rarely used.

---

## Shared spine: exactly what enters each hash

All three candidates share this accounting; per-candidate deltas are
called out inline. This is the section to review hardest.

### 1. Patched dependency → the `package_id`

`ConfigHashInputs.package_id` today is the git commit sha or
`"blake3:<hex>"` for url deps. Patches fold into the **package id**, not
into a new hash field:

```
package_id  =  <base>                                      (no patches)
package_id  =  <base> "+patches:" blake3_32(patch_stream)  (patches)

patch_stream = for each patch, in manifest order:
                 u64-LE(byte length) || raw patch-file bytes
```

`<base>` = the resolved commit sha (git) or `blake3:<hex>` (url),
unchanged. `blake3_32` = first 32 hex chars, same truncation the config
hash uses. Length-prefixed concatenation matches the collision-proofing
convention already documented in `hashing.rs`.

Why the package-id route and not a new `ConfigHashInputs` field:

- **Semantically right:** patches change *source content identity*. Two
  deps at the same commit with different patches are different sources,
  exactly like two commits.
- **No store invalidation:** the `cppkg-config-hash-v1` encoding is
  untouched; every existing (unpatched) hash is unchanged. Adding a field
  would re-key the entire store for a feature most entries never use.
- **The raw store keys off the same id** (`store::raw_dir`), so a patched
  checkout gets its own raw entry (`absl-255c84dadd029fd8+…` vs the
  pristine `absl-255c84dadd029fd8`) and never contaminates the pristine
  cache. (`raw_dir` shortening must incorporate the patch suffix — see
  implementation sketch.)

This is the fix for the observed per-machine hash split
(`fee068f7` vs `f4632513`): the hash inputs become *base commit + patch
bytes*, both of which live in the consumer's VCS. Store keys and
lockfiles are shareable again.

**Lockfile** (additive, grammar pinned like the rest of the lock ABI):

```toml
[[package]]
name = "absl"
source = "git+https://github.com/abseil/abseil-cpp"
requested = "tag:20260107.1"
commit = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"
patches = ["blake3:<full hex of patch 1>", "..."]   # in application order
```

`patches` present iff the dep declares patches. Resolve re-verifies the
recorded hashes against the current patch files; drift → re-resolve (the
lockfile updates, the config hash changes, the dep rebuilds — by design).

### 2. System dependency → a manifest-only store entry with its own hash

(Candidates A and B only; C defers the form.) A system dep is never
built; it is *resolved*, and the resolution is captured as a store
manifest whose hash contributes to dependents exactly like a built dep's:

```
sysdep_hash = blake3_32(
    "cppkg-sysdep-v1"
 || put_str(dep key)
 || put_str(resolution mode)            # "cmake" | "pkg-config"
 || put_str(resolved version string)    # "" if unknown
 || put_list(sorted resolved library absolute paths)
 || put_list(blake3 of each resolved library file's bytes, same order)
 || put_list(sorted resolved include dirs)
)
```

- The sysdep hash enters dependents' `dep_hashes` (via `needs` /
  target-dependency edges) — **unchanged plumbing**, Nix-style
  transitivity does the rest.
- Library **file bytes** are hashed (cheap: a few MB), so a Homebrew /
  pacman upgrade that replaces `libzstd` re-keys every dependent even
  when the version string lies. Header contents are *not* hashed
  (include trees are big; drift there is an accepted, documented gap —
  consistent with "hermeticity is best-effort and documented as such",
  CPP_PKG_IMPLEMENTATION.md §3).
- Consequence, stated plainly: **store entries downstream of a system dep
  are machine-local by construction.** That is the *correct* version of
  what cpptrace observed — today two machines produce *different
  artifacts under the same key* (silent lie); with this, they produce
  different keys (truth). Shareability is preserved exactly for the
  hermetic subgraph.

### 3. Builtin pseudo-packages (Threads) → nothing new enters any hash

`Threads::Threads` expansion is a **pure function of the toolchain
identity, which is already a hash input** (`ConfigHashInputs.toolchain`).
So the builtin contributes no new bytes: darwin → nothing (libSystem),
linux-gnu → `-pthread` (compile + link), msvc → nothing. Extracted store
manifests record the symbolic link input `builtin:threads` instead of
whatever the probing machine's FindThreads answered — which makes those
manifests *more* platform-portable than today, not less.

### 4. Hermeticity scan → not a hash input; a truth check on hash coverage

The scan's job is to guarantee an invariant the store already claims:
*every absolute path recorded in a store manifest is covered by some hash
input*. Store-rooted paths are covered by `dep_hashes`; declared-system
paths are covered by the sysdep hash; anything else is a lie and gets
diagnosed. The scan never feeds the hash — it polices it.

---

## Candidate A — one table: `system = true` inside `[dependencies]` (recommended)

One idea: a dependency is a dependency; *where it comes from* is the
source form. `system = true` joins `git`/`url` as a third source form.
Patches are a plain string list on fetched sources.

### TOML surface

```toml
[dependencies.absl]
git     = "https://github.com/abseil/abseil-cpp"
tag     = "20260107.1"
patches = ["patches/absl-0001-mark-heterogeneous_lookup_testing-TESTONLY.patch"]
options = { ABSL_ENABLE_INSTALL = "ON", ABSL_PROPAGATE_CXX_STD = "ON" }

[dependencies.zstd]
system      = true            # resolve from the machine, never build
min-version = "1.5"           # optional; checked against resolved version

[targets.bench]
dependencies = ["benchmark::benchmark", "Threads::Threads"]  # builtin, no decl
```

### Semantics — `patches`

- **Value:** array of paths, relative to the directory containing
  `CppPkg.toml`. Files must exist at resolve time (missing → error naming
  the dep and path). Duplicate entries → error (a patch cannot apply
  twice). Empty array ≡ absent.
- **Valid on** `git` and `url` sources, including future
  dev-dependencies (B2) — identical behavior. Invalid on `system = true`
  (error: "system dependencies have no source tree to patch"). Future
  `path` deps: patches follow the source, out of v-next scope, recorded
  non-foreclosure: nothing here assumes store-resident sources.
- **Application:** after fetch/extract, before anything reads the tree
  (before the B12 `subdir` root is entered, before configure). Applied in
  manifest order via `git apply -p1 --whitespace=nowarn` run at the
  checkout root (works in non-repo dirs, so url tarballs are covered by
  the same code path). Strictness is git-apply's native contract:
  **context must match exactly, zero fuzz; pure line-offset drift is
  tolerated**; paths escaping the tree (`../`) are rejected by git apply;
  git binary patches are allowed (they are content-exact by nature).
- **Failure:** any hunk failure → hard error citing dep, patch file, and
  the failed hunk header, with the hint that a version bump likely
  invalidated the patch context ("re-diff against <resolved commit>").
  No `--3way`, no partial application: the patched raw entry is created
  atomically (apply into temp dir, rename on success — same
  incomplete-entry discipline the store already uses).
- **Patched raw entry:** keyed by the composed package id (§ spine 1);
  the pristine raw entry is never mutated. `cpp-pkg` prints the composed
  id in build output so "am I on the patched source?" is answerable.
- **Hash:** § spine 1. Editing patch bytes or reordering the list re-keys
  the dep and every dependent. Renaming a patch *file* without changing
  bytes does **not** re-key (bytes are hashed, not names) — deliberate.

### Semantics — `system = true`

- Mutually exclusive with `git`/`url` fields, `patches`, and `options`
  (all → error; a system package has no cache to configure). `needs` on a
  system dep is an error (its config's own find_dependency calls run
  against the same machine). Other deps may list a system dep in *their*
  `needs`, and target `dependencies` may reference its exported targets —
  unchanged plumbing.
- **Resolution (mode "cmake"):** the existing tier-2 probe runs
  `find_package(<find-package or key>)` — but with the hermetic find
  restrictions *opened* for this one package (system prefixes allowed;
  everything else stays scrubbed). Extraction reuses `probe.rs`
  wholesale; imported targets, defines, includes, link paths land in a
  normal store manifest, flagged `system = true`. Resolved version =
  CMake's `<pkg>_VERSION` if set.
- **No artifacts.** The store entry is manifest-only
  (`pkg/<key>-<sysdep_hash>/` containing `cppkg-manifest.json` + marker).
  Re-resolution happens when the recorded file hashes no longer match the
  files on disk (cheap stat+hash check per build).
- **Failure:** not found → error with both worlds' fixes: "declare it as
  a fetched dependency (git/url) to build hermetically, or install
  <name> (`pacman -S zstd` / `brew install zstd`)". `min-version`
  unsatisfied or unknown-when-required → error naming resolved vs
  required.
- **Hash:** § spine 2. On this dep's dependents, nothing changes shape —
  `dep_hashes` just contains a sysdep hash.

### Semantics — builtin pseudo-packages

- v1 builtin list: **`Threads::Threads` only** (the only pseudo-package
  wave 1 ever hit; `dl`/`m`/`rt` are link flags on Linux and belong to
  B3 cfg link-flags until evidence says otherwise).
- **Native side:** `Threads::Threads` is referenceable in any target's
  `dependencies` with no `[dependencies]` entry — resolution ladder gets
  a step 0: builtin names resolve first and cannot be shadowed
  (`exposes-targets = ["Threads::Threads"]` becomes an **error**:
  "builtin pseudo-package; delete this line" — migration note in the
  release notes, since three wave-1 manifests carry exactly that line).
- **Extraction side:** an imported target named `Threads::Threads`
  appearing in a probe diff is dropped from ownership attribution and its
  uses are rewritten to the symbolic link input `builtin:threads`. Sanity
  check: its extracted interface must be one of the known shapes (empty /
  `-pthread` / a pthread library path); anything else → warning, literal
  interface kept (never silently discard information).
- **Expansion:** at plan/ninja-gen time, from toolchain identity:
  linux-gnu adds `-pthread` to compile and link lines of every target
  whose closure contains the builtin; darwin/msvc add nothing. Interface
  semantics match CMake's `Threads::Threads` (usage requirement, so it
  propagates like a public dep of whoever references it).

### Semantics — hermeticity scan

- **Where:** two layers. (1) `manifest::from_probe` post-pass: every
  absolute path in link libraries / include dirs / (macOS) framework
  paths of an extracted manifest must be under the store root **or**
  belong to a declared system dep's resolved path set. (2)
  `check_find_package_leaks` grows beyond `*_DIR` config-mode hits to
  scan `*_LIBRARY`/`*_INCLUDE_DIR`-shaped cache entries — the
  find_library/module route is exactly how cpptrace's zstd leak evaded
  the current scan; declared system deps' paths are allowlisted.
- **Policy:** **error** by default. The message names the dep, the leaked
  path, and both fixes: "declare `[dependencies.zstd] system = true`, or
  disable the feature (e.g. `ENABLE_DECOMPRESSION = \"FALSE\"`)".
  Downgrade: `cpp-pkg build --allow-undeclared-system-libs` (warn,
  documented as unsupported-for-sharing). No manifest knob — a manifest
  that permanently declares "I lie" fails taste tie-breaker (1).
- SDK/toolchain-rooted absolute paths (e.g. the macOS SDK `libz.tbd`)
  are **not** exempt: undeclared is undeclared; the fix is one
  `system = true` line. (Toolchain-internal paths that never enter
  manifests — compiler runtimes — are invisible to the scan by nature.)

### Corpus use sites — before → after

**vtz, absl TESTONLY (the surviving patch case).** Before
(`migrations/vtz/CppPkg.toml` + 40 lines of pin.sh clone/commit
machinery + placeholder substitution):

```toml
[dependencies.absl]
git = "@ABSL_PATCHED_REPO@"          # pin.sh sed-substitutes a file:// URL
rev = "4645a01a5cee98f8a95b83b0b7c8acd5a3ed93a1"   # per-machine synthetic commit
```

After (manifest literally buildable as checked in; pin.sh dep section
deleted; `patches/` file already exists in the repo):

```toml
[dependencies.absl]
git     = "https://github.com/abseil/abseil-cpp"
rev     = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"    # = tag 20260107.1
patches = ["patches/deps-absl-0001-mark-heterogeneous_lookup_testing-TESTONLY.patch"]
options = { ABSL_ENABLE_INSTALL = "ON", ABSL_PROPAGATE_CXX_STD = "ON", CMAKE_CXX_STANDARD = "20" }
```

**vtz, date INTERFACE_SOURCES.** Before: second patched clone
(`@DATE_PATCHED_REPO@` + synthetic commit). After: **no patch at all** —
tool fix (2) makes upstream v3.0.4 consumable:

```toml
[dependencies.date]
git = "https://github.com/HowardHinnant/date"
tag = "v3.0.4"
options = { BUILD_TZ_LIB = "ON", MANUAL_TZ_DB = "ON" }
```

**abseil consumer, self-edge.** Before
(`migrations/abseil/consumer/CppPkg.toml`): `git =
"file://@ABSL_PATCHED_REPO@"` + local tag `20260526.0-cppkg1` + the
checked-in `0001-remove-absl-strings-self-dep.patch` applied by pin.sh.
After: **no patch** — tool fix (1):

```toml
[dependencies.absl]
git = "https://github.com/abseil/abseil-cpp"
tag = "20260526.0"
```

**Threads ownership dance** (vtz `[dependencies.date]`, json-tui
`[dependencies.ftxui]`, would-be googletest/benchmark/ninja consumers).
Before: `exposes-targets = ["Threads::Threads"]` on an arbitrary owner +
a 6-line apology comment. After: the line is **deleted** (and now
rejected). Native ports of benchmark/ninja_test additionally *gain* the
upstream edge they currently drop:

```toml
[targets.benchmark]
dependencies = { private = ["Threads::Threads"] }   # upstream parity; -pthread on Linux
```

**cpptrace, zstd leak.** Before: `ENABLE_DECOMPRESSION = "FALSE"` scope
reduction, because TRUE silently baked
`/opt/homebrew/lib/libzstd.dylib` into a store manifest. After — two
honest spellings, user's choice, and the *silent* third option is gone
(the scan errors on it):

```toml
# hermetic (needs B12 subdir; the store builds zstd):
[dependencies.zstd]
git = "https://github.com/facebook/zstd"
tag = "v1.5.7"
subdir = "build/cmake"
# — or system:
[dependencies.zstd]
system = true

[dependencies.libdwarf]
git = "https://github.com/jeremy-rifkin/libdwarf-lite"
rev = "5dfb2cd2aacf2bf473e5bfea79e41289f88b3a5f"
needs = ["zstd"]
options = { PIC_ALWAYS = "TRUE", BUILD_DWARFDUMP = "FALSE", ENABLE_DECOMPRESSION = "TRUE" }
```

**cppcheck, HAVE_RULES / PCRE** (no CMake config exists to probe;
today: feature unmigrateable). After: `pcre = { system = true }` resolves
via the FindPCRE-style module path or — if no module exists either —
this is the case that motivates Candidate B's pkg-config mode; under A
it needs a find-module and is otherwise still out (honest limit, noted
in costs). cppcheck's ambient-Boost auto-detection maps to
`boost = { system = true }` + `USE_BOOST = "On"` — declared instead of
ambient.

### Linux story (gcc 16 / clang 22, Arch)

- Patches: byte-identical behavior; `git` is already a hard dependency
  of the fetch layer. Nothing platform-specific.
- Threads builtin: linux-gnu expansion `-pthread` on compile and link
  (correct for glibc ≥ 2.34 where libpthread merged — `-pthread` remains
  the blessed spelling; also correct for older glibc). This is the piece
  that makes every wave-1 manifest's *missing* Threads edge stop being a
  silent macOS-ism.
- System deps: Arch is the friendly case — one package universe under
  `/usr`, pkg-config files for nearly everything, CMake configs for much
  of it. Resolution mode "cmake" works unchanged; version strings are
  reliably present. The sysdep file-hash check makes `pacman -Syu`
  correctly invalidate dependents.
- Hermeticity scan: **more** important on Linux, where `/usr/lib` hits
  are the default failure mode of any find_library — the scan is what
  keeps an Arch store honest at all.

### Implementation sketch

- `schema.rs`: `patches: Vec<String>` + `system: bool` +
  `min_version: Option<String>` on `DependencySpec`;
  `SourceSpec::System`; validation matrix above; builtin-name step in
  the resolution ladder; reject `exposes-*` of builtin names.
- `fetch.rs`: `apply_patches(dep_key, workdir, &[PathBuf]) -> Result<Vec<PatchHash>>`
  (git apply, temp-dir + rename); compose package id.
- `hashing.rs`: `patched_package_id(base, patch_stream)`,
  `sysdep_hash(...)` (new domain tags; `cppkg-config-hash-v1` encoding
  untouched).
- `store.rs`: `raw_dir` incorporates the patch suffix into the shortened
  label; manifest-only entry flavor for sysdeps.
- `lockfile.rs`: `patches` array; `source = "system"` rows with
  `resolved-version` (informational; the hash lives in the store entry).
- `cmake_build.rs`: per-package find-gate opening for declared system
  deps; `check_find_package_leaks` extended to `*_LIBRARY`/
  `*_INCLUDE_DIR` cache shapes with sysdep allowlist.
- `probe.rs`/`manifest.rs`: sysdep resolution probe (reuse tier-2);
  `builtin:threads` normalization + shape sanity check; hermeticity
  post-pass over extracted manifests.
- `graph.rs`/`ninja_gen.rs`: `LinkInput::Builtin(BuiltinPkg)` variant;
  toolchain-conditional expansion.
- `cli.rs`: diagnostics, `--allow-undeclared-system-libs`.

### Costs (honest)

- `system = true` resolution quality is bounded by CMake's find
  ecosystem; the PCRE-class library (no config, no module) is still out.
- Sysdep hashing covers lib bytes but not header trees: a header-only
  system change (rare for system libs, real for system Boost) can slip
  a stale hash. Documented gap.
- Builtin list is a curated vocabulary — the same "chases reality
  forever" critique as S1-B. Mitigation: entries added only on migration
  evidence, and Threads alone covered 6/8 projects.
- The find-gate ("open hermetic find restrictions for exactly this
  package") is fiddly CMake-mechanics code with edge cases (transitive
  find_dependency from a system package's own config re-opens the
  world). Engineering-heavy corner.
- One new source form in the one table means `[dependencies]` entries
  are no longer uniformly "fetch + build" — readers must notice
  `system = true`. (This is Candidate B's argument.)

---

## Candidate B — two tables: `[system-dependencies]` + pkg-config-first

Segregate by trust boundary: `[dependencies]` is the hermetic world the
store owns; `[system-dependencies]` is the declared hole in it. Resolve
system deps via **pkg-config first** (the native Unix source of truth),
CMake module fallback. Patches identical to A but with a reserved
string-or-table entry form.

### TOML surface

```toml
[dependencies.absl]
git = "https://github.com/abseil/abseil-cpp"
rev = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"
patches = [
  "patches/absl-0001-testonly.patch",                      # sugar (string)
  { file = "patches/odd-layout.patch", strip = 2 },        # reserved table form
]

[system-dependencies]
zstd = { pkg-config = "libzstd", min-version = "1.5" }
pcre = { pkg-config = "libpcre" }
threads = {}          # well-known name; no pkg-config lookup, builtin semantics

[targets.app]
dependencies = ["core", "zstd"]    # system-dep keys are directly referenceable
```

### Semantics (deltas from A only)

- **Namespaces:** `[dependencies]` and `[system-dependencies]` share one
  key namespace (collision → error). `needs` may reference keys of
  either table. A system-dep key used in target `dependencies` refers to
  the whole resolved package (its single synthesized component), so no
  `::` names are needed for the pkg-config case.
- **Resolution:** `pkg-config --modversion/--cflags --libs <name>`;
  parsed into includes / defines / link inputs; recorded into the same
  manifest-only store entry with the same `sysdep_hash` (§ spine 2,
  mode "pkg-config"). Fallback `find-package = "<CMakeName>"` field
  switches an entry to the tier-2 probe route when a library only speaks
  CMake. `threads` (and only well-known names) may have an empty body.
- **Patches table form:** `strip` (default 1) is the only defined key;
  everything else reserved. Hash = patch bytes regardless of form;
  `strip` is hashed alongside (it changes application semantics):
  `patch_stream` entries become `u64(strip) || u64(len) || bytes`.
- Threads builtin/dedup and the hermeticity scan are **identical to A**
  (the builtin exists whether or not `threads = {}` is declared;
  declaring it is a no-op that documents intent).

### Corpus after (deltas)

- vtz/abseil patch sites: same as A modulo table form availability.
- cpptrace: `[system-dependencies] zstd = { pkg-config = "libzstd" }` —
  works on macOS too (Homebrew ships `libzstd.pc`).
- **cppcheck PCRE: solved** — `pcre = { pkg-config = "libpcre" }` is the
  one wave-1 case A cannot express (no CMake config *or* module usable);
  this is B's strongest concrete win.
- Threads lines: as A.

### Linux story

Strictly the best of the three: pkg-config is the native protocol of the
platform (Arch ships `.pc` files pervasively); no CMake involved in
resolving a system lib; `min-version` maps to
`pkg-config --atleast-version`. macOS is the weaker side: SDK-only
libraries (libz.tbd, libcurl) have no `.pc` in the SDK — those need the
`find-package` fallback or stay undeclarable.

### Implementation sketch (deltas)

New `pkgconfig.rs` resolver (exec + parse; ~150 lines); schema gets a
second table + cross-table key validation; `needs`/reference plumbing
taught about the second table. Everything else as A.

### Costs (honest)

- **Two tables** for one concept ("this project needs zstd") — taste
  tie-breaker (3) ("one orthogonal primitive over two special cases")
  cuts against it; Cargo has no analog (its system-dep story lives in
  build.rs, which is exactly what we refuse to have).
- Two resolution modes (pkg-config + CMake fallback) from day one =
  double the resolver surface, double the failure modes to translate.
- The synthesized single-component model is a projection: pkg-config has
  no component structure, but CMake system packages do (e.g. system
  ICU); B eventually re-grows the probe route A starts with.
- Reserved table form on patches buys `strip = 2` (no wave-1 evidence
  needs it) at the cost of the schema's least-minimal pattern
  (mixed string-or-table lists) — S3-B's own critique.

---

## Candidate C — escape-hatch minimum: patches + builtin Threads + warn-only scan; defer the system-dep form

The BACKLOG's own Arrow plan says the system-vs-bundled toggle is
expected to be *Arrow's headline gap* and to "produce design data for it
rather than pre-building it". C takes that seriously: ship the pieces
with zero design risk now, and let Arrow write the sysdep spec.

### TOML surface

```toml
[dependencies.absl]
git     = "https://github.com/abseil/abseil-cpp"
rev     = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"
patches = ["patches/absl-0001-testonly.patch"]     # exactly Candidate A's field

[targets.bench]
dependencies = { private = ["Threads::Threads"] }  # builtin, as in A
```

No `system = true`, no `[system-dependencies]`.

### Semantics

- `patches`: identical to Candidate A, verbatim (strings only, git
  apply -p1, package-id composition, lockfile rows).
- Builtin Threads dedup: identical to A.
- Hermeticity scan: same detection machinery as A (manifest post-pass +
  extended cache scan) but **warn**, not error — because without a
  declaration form, the error would have no in-schema fix to suggest,
  only "turn the feature off". Every warning is a logged design datum
  for the Arrow-informed sysdep design; the flag flips to error the day
  the declaration form lands.
- `-lrt` / `-ldl` / `-lm`-class needs route to **B3 cfg link-flags**
  (`linux` predicate), which wave-1 evidence (abseil/benchmark LINKOPTS)
  already demands from the cfg design regardless.

### Corpus after

vtz absl, abseil consumer, Threads lines: as A. cpptrace: stays
`ENABLE_DECOMPRESSION = "FALSE"` on macOS, **but now with a warning**
if anyone flips it — the silent leak is dead, the feature gap remains.
cppcheck PCRE/Boost: still unmigrateable (deferred, on record).

### Linux story

Patches/Threads as A. The gap: a Linux cpptrace build genuinely wants
zstd decompression (ELF compressed sections — the exact scope reduction
cpptrace GAPS flags), and C's only hermetic answer is
"declare zstd as a fetched dep once B12 `subdir` lands" — which does
work, builds zstd from source in the store, and is arguably the more
cpp-pkg-native answer anyway. What C cannot do on Linux: link the
distro's zstd. Arch users will notice.

### Implementation sketch

Strict subset of A: no sysdep resolver, no find-gate, no manifest-only
store entries. The smallest diff of the three by a wide margin.

### Costs (honest)

- Punts B7(3) — BACKLOG lists it "strongly wanted" for Arrow; if the
  deferral is wrong, Arrow's migration stalls on a design round instead
  of an implementation round.
- Warn-only scan means the machine-dependent-store-entry lie remains
  *possible* (though no longer silent) until wave 2.
- Two migrations (cppcheck rules mode, Linux cpptrace-with-system-zstd)
  stay unwritable.

---

## Interaction analysis (explicit, per overlapping area)

- **Flags (B1/S1):** builtin Threads expands to `-pthread` — emitted as
  toolchain-level compile/link inputs, ordered *before* target
  `cxx-flags`/`link-flags` so last-wins user overrides (documented
  contract per cppcheck GAPS) still hold. No new flag surface is added
  by this area; system deps contribute includes as `-isystem` (riding
  the decided imported-interface-includes-are-system fix — system-dep
  headers are the textbook case for it).
- **cfg (B3/S3):** deliberate division of labor: *link a platform lib
  the platform way* → cfg link-flags (`-lrt` under `linux`); *link a
  package that exists on all platforms* → dependency edge
  (`Threads::Threads`, `zstd`). Rule of thumb for the docs: if it has
  headers or a version, it is a dependency; if it is a linker spelling,
  it is a cfg flag. Also: `[dependencies.X.cfg.linux]`-style
  conditional deps (S3-A) must compose with `system = true` (a dep that
  is fetched on macOS, system on Linux, is Arrow's bundled-vs-system
  idiom in miniature — the one-table Candidate A makes that a cfg merge
  on one entry; two-table B needs cross-table cfg, which is ugly —
  flagged for the judge).
- **codegen (B4/S4):** `${pin.<dep>.commit}` interpolation reports the
  **base commit**, never the composed package id — patches don't move
  the upstream commit, and version-stamping codegen (benchmark) wants
  the upstream identity. A `${pin.<dep>.patched}` boolean fact is cheap
  to add if a template ever needs to advertise it. Nothing in this area
  executes commands; no hermeticity interaction with tier-2 `[generate]`
  beyond the existing rule that generated outputs live under `${gen}`.
- **tests (B2/S2):** `patches` and `system` deps are legal on
  `[dev-dependencies]` with identical semantics — vtz's patched absl is
  *already* test-only (`vtz_testing` closure), so on the day B2 lands
  that patch moves tables without changing meaning or hash. The B2
  runner needs no awareness of this area.
- **install/export (B6):** exporting a package whose closure contains a
  system dep must record the requirement (a `requires: zstd >= 1.5,
  system` row in `cppkg-manifest.json` / the Config shim's
  `find_dependency`) rather than baking resolved paths into the export.
  Not designed here; constraint handed to B6: *the manifest-only sysdep
  store entry is the record B6 should serialize from.*
- **B12 `subdir`:** ordering is fetch → patch → subdir-selects-configure-
  root; patches are rooted at the checkout root (`-p1` from the repo
  top), NOT the subdir — so one patch can touch `build/cmake/` and
  `lib/` together. `subdir` hashes as a literal string in B12's design;
  no overlap with the patch stream.
- **`find-package` field (B12 doc item):** Candidate A's system deps
  reuse it verbatim for resolution; documenting it (B12) becomes
  load-bearing for this area.

## Costs common to all candidates

- Patch maintenance across dep bumps is real (context rot); the error
  message can only make re-diffing easy, not unnecessary. (Overlay dirs
  were considered and rejected in S5 — they rot worse across bumps and
  hide the change; not re-litigated here.)
- `git apply` becomes a correctness-critical dependency of the fetch
  layer (it already is for cloning; still worth stating).
- The composed package id lengthens raw-store dir names; shortening must
  keep base-commit and patch-hash segments distinguishable
  (`absl-255c84da+a1b2c3d4`).
- Sysdep hashing (A/B) makes "why did my dep rebuild" occasionally
  answer "your OS updated libzstd" — correct but surprising; the CLI
  should say so explicitly when a sysdep hash mismatch triggers
  invalidation.

---

## OPEN QUESTIONS for the taste judge

1. **One table or two?** `system = true` as a third source form inside
   `[dependencies]` (A) vs a segregated `[system-dependencies]` (B).
   Tie-breaker (3) favors A; "the hermetic table stays pure" favors B.
2. **Ship a system-dep form now, or defer to Arrow (C)?** BACKLOG both
   demands it ("strongly wanted") and advises mining Arrow for its
   design. If deferred: is warn-only hermeticity acceptable for a full
   campaign stage on Linux, where system libs are the default idiom?
3. **Hermeticity default when a declaration form *does* exist:** error
   (A/B as specced) or warn-first-release? Error is the only choice
   under tie-breaker (1), but it will fire on day one of Linux bring-up.
4. **Sysdep hash strength:** resolved-path + version + *lib file bytes*
   (specced) vs path + version only. Bytes are strictly truer and cheap;
   any reason to prefer the weaker identity?
5. **Resolution protocol:** CMake-probe-first (A) vs pkg-config-first
   (B)? Decides whether cppcheck's PCRE case is in or out at v1, and
   how macOS SDK-only libs are declared.
6. **Patch entries:** bare strings forever, or reserve the
   string-or-table form (`{ file, strip }`) now, consistent with the
   deps-array precedent? No wave-1 evidence needs `strip ≠ 1`.
7. **Builtin list v1 = Threads only** — confirm, or seed `dl`/`m`/`rt`
   as builtins instead of routing them to cfg link-flags?
8. **Spelling of the builtin reference:** keep the CMake-shaped
   `Threads::Threads` (extraction-identical, C++-native familiarity) or
   introduce a bare `threads` (cargo-flavored, but then two names mean
   one thing the day extraction meets a native reference)?
9. **`exposes-targets` of a builtin becomes an error** (breaks three
   wave-1 manifests, trivially fixed by deletion) — acceptable, or
   should it be a warning for one release?
