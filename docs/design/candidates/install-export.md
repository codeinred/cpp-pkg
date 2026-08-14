# Design candidates — install-export (B6), + B8 / B9 recommendations

Area: **B6 install/export** (producers are a cul-de-sac — 8/8 projects,
gates all library adoption), plus one-paragraph recommendations for the two
ratify-level surfaces folded in here: **B8 glob exclusion** and **B9
target-defaults**.

Evidence base: BACKLOG.md §B6/§B8/§B9; GAPS.md sections — vtz §8 (FILE_SET
HEADERS api/impl split), cppcheck §3 (runtime data silently broken until
staged; FILESDIR prefix-define feedback), googletest §5/§6 (ecosystem loses
`find_package(GTest)` if googletest adopts CppPkg.toml; version metadata),
benchmark §6 (Config + version file + pc files; round-trip fixpoint test),
cpptrace §9 (static-closure question: upstream vendors `libdwarf.a` into its
own prefix), abseil install-export ("the port is a cul-de-sac
artifact-wise"; round-trip idea: emit Config shims for *local* targets),
ninja §7 + json-tui §8 (single-binary `install` verb; 1 of 9 ninja
executables is a product).

---

## 0. The shared spine (engineering-dominated; common to all candidates)

Every candidate below shares this core. The taste question is *how products,
headers, and data are declared*, not whether this machinery exists.

### 0.1 The verb

```
cpp-pkg install --prefix <dir> [--destdir <dir>] [--config release]
                [--toolchain <t>] [targets...]
```

- Builds (respecting `--config`/`--toolchain` exactly like `cpp-pkg build`),
  then stages into `<destdir><prefix>` with the standard layout:

```
<prefix>/bin/<exe>                                   executables
<prefix>/lib/lib<target>.a                           archives
<prefix>/include/...                                 public headers
<prefix>/lib/cmake/<CmakeName>/<CmakeName>Config.cmake
<prefix>/lib/cmake/<CmakeName>/<CmakeName>ConfigVersion.cmake
<prefix>/lib/cmake/<CmakeName>/cppkg-manifest.json   the manifest itself
<prefix>/share/<package>/<...>                       runtime data
```

- `--destdir` (also honors the `DESTDIR` env var) stages into
  `<destdir><prefix>` while all *baked-in* paths (Config internals,
  `${install-prefix}` defines) refer to `<prefix>` — the standard
  distro-packaging contract. This costs one string concatenation and buys
  Arch/Debian packageability on day one; Linux packagers will not adopt a
  tool without it.
- Install is idempotent and overwrite-by-default; it never deletes files it
  did not just write (no `uninstall` in v1).

### 0.2 Export emission — the round-trip fixpoint

The **existing shim emitter (`shim.rs`) pointed at the project's own
manifest**, exactly as the BACKLOG sketch says. `cpp-pkg install` constructs
a `manifest::Manifest` from the local `[targets]` graph (only the exported
subset) and emits:

1. `<CmakeName>Config.cmake` — IMPORTED targets under the export namespace
   (`vtz::vtz`, `GTest::gtest`), with `INTERFACE_INCLUDE_DIRECTORIES`
   pointing at `<prefix>/include`, `IMPORTED_LOCATION_<CONFIG>` at
   `<prefix>/lib/...`, public defines/flags/cxx-std as
   `INTERFACE_COMPILE_DEFINITIONS/OPTIONS/FEATURES`, private static-lib deps
   as `$<LINK_ONLY:...>` — the property spelling `probe.rs` already reads.
   **Relocatable**: paths are emitted relative to
   `${CMAKE_CURRENT_LIST_DIR}/../../..` (CMake's own `_IMPORT_PREFIX`
   pattern), never absolute.
2. `<CmakeName>ConfigVersion.cmake` — real compatibility policy now that we
   are a producer: **SameMajorVersion** semantics generated from
   `[package].version` (benchmark and cpptrace both ship exactly this).
   Exporting a library package with no `version` is an error; a
   binaries-only install (ninja, json-tui) needs no version.
3. `cppkg-manifest.json` — the same manifest serialized beside the Config,
   with paths written against a `@prefix@` placeholder (CPS precedent)
   resolved by the reader relative to the file's location. A future
   `prefix`-form dependency (open question Q7) reads this directly and
   skips the probe; a CMake consumer never sees it.

**Fixpoint invariant (the acceptance test):** probing the installed
`Config.cmake` with the existing tier-2 probe must reproduce
`cppkg-manifest.json` exactly (modulo prefix resolution). `shim.rs` already
documents and tests this invariant for dependency shims; install extends the
same test to local targets. benchmark GAPS §6 names this test explicitly.

### 0.3 External dependencies of exported targets (the cpptrace question)

An exported target's closure may contain: (a) other exported targets of this
package — emitted as sibling IMPORTED targets in the same Config; (b) local
targets **not** exported — **hard error** at install time ("target `X` is in
the link closure of exported target `Y`; add `install = true` to `X` or
remove the edge") — same rule CMake enforces for export sets, and the
silent alternative is a broken archive; (c) external package deps — emitted
as `find_dependency(<FindName> <version>)` in the Config **and** recorded in
`cppkg-manifest.json`'s `requires` block with the lockfile pin (source URL +
commit/content-hash + options). A CMake consumer resolves them from its own
environment, as with any CMake package; a cpp-pkg consumer can re-provision
the *identical* dependency from the recorded pin. (d) system libs and
frameworks — recorded as-is (they are the consumer machine's business).

**Vendoring the static closure (what cpptrace upstream does — copying
`libdwarf.a` into its own prefix) is explicitly deferred.** It is a
real-world pattern, but it duplicates artifacts, hides provenance, and
collides with the store's identity model; the `requires`+pins answer keeps
the declarative reading honest. Recorded as a cost, revisit if wave-2 shows
prefixes being shipped to dep-less machines. (Open question Q8.)

Dev-dependencies (B2) in an exported closure are a hard error — the
export/default graph is exactly what `[dev-dependencies]` exists to stay
out of.

### 0.4 Runtime data (the cppcheck fix — separable, ships first)

Identical in all three candidates because it is **not only an install
concern**: cppcheck is silently broken *in the build tree* until data is
staged next to the binary (observed: zero findings, clean exit).

```toml
[targets.cppcheck]
runtime-data = [
  { from = "cfg",       patterns = ["*.cfg"] },
  { from = "platforms", patterns = ["*.xml", "!*-unsigned.xml"] },
  { from = "addons" },                       # whole dir; default patterns ["**/*"]
]
```

Semantics:

- `from` — source-tree directory, required; missing directory is a hard
  error (never silently empty).
- `patterns` — globs relative to `from`, `!`-negations per B8 (one grammar
  everywhere); default `["**/*"]`. This directly covers upstream's
  `remove_unsigned_platforms` pruning as an exclusion instead of a copy
  step.
- `to` — destination subdirectory; **defaults to the last path component of
  `from`**. Two entries writing the same destination file is a hard error.
- **Build time:** files are copied to `<build>/<to>/...` — next to the
  target's output — via ordinary ninja copy edges (inputs hashed by
  ninja's mtime tracking; globs re-expand at generation time like
  `sources`, so new upstream files are picked up). This alone dissolves
  `stage-data.sh` and the silent-broken shape.
- **Install time:** the same set lands under `<prefix>/share/<package>/<to>/...`.
- Runtime data participates in *no* compile or link edge; it is
  order-only-attached to the owning target so `cpp-pkg build cppcheck`
  always stages it.

### 0.5 Prefix-define feedback (`${install-prefix}`)

cppcheck compiles `FILESDIR="/usr/local/share/Cppcheck"` into the binary —
the install prefix is a *compile-time input*. The escape:

```toml
[targets.cppcheck]
defines = { private = ['FILESDIR="${install-prefix}/share/cppcheck"'] }
```

- `${install-prefix}` is a manifest interpolation variable usable in
  **define values only** (v1), sharing the single `${...}` grammar with
  B4's `${package.*}`/`${pin.*}`/`${gen}` — one interpolation namespace,
  coordinated with the codegen design; no second syntax.
- Value: the `--prefix` given to `cpp-pkg install`; plain `cpp-pkg build`
  uses the default `/usr/local` (overridable with `build --prefix` for
  parity testing). Changing the prefix changes the compile command for
  affected TUs → ninja rebuilds exactly those. Deps are untouched (project
  TUs only), so store keys are unaffected.
- Dev-tree behavior is honest by composition: the baked `/usr/local` path
  usually doesn't exist in a dev tree, cppcheck falls back to its
  exe-directory search, and §0.4 staged the data exactly there. Installed
  behavior: `install --prefix /opt/x` rebuilds the one TU that embeds
  FILESDIR, then stages `share/cppcheck` — both search paths now true.

### 0.6 What install does NOT do in v1 (honest scope)

- No `.pc` pkg-config emission (benchmark/googletest ship them). Deferred,
  but flagged: this is a *Linux-relevant* gap — distro consumers of
  gtest/benchmark use pkg-config. Cheap to add later from the same
  manifest; open question Q6 asks whether it should ride along now.
- No shared libraries (schema has no `shared-library` kind yet) — so no
  SONAME/VERSION questions; googletest's `VERSION` property need is
  covered by ConfigVersion + manifest version metadata only.
- No CPack-style DEB/RPM/DMG (json-tui §8) — out of scope, DESTDIR is the
  packager interface.
- No executable export as IMPORTED CMake targets (only *installation* of
  executables). Codegen-tool packages (protoc-style) will want it; open
  question Q3.

---

## Candidate A — "Declared products, declared headers" (explicit FILE_SET analog)

Everything installed is spelled in the manifest, on the target it belongs
to. Nothing is derived.

### TOML surface

```toml
[package]
name    = "vtz"
version = "1.4.0"

[export]                          # optional table; all keys have defaults
cmake-name = "vtz"                # Config file name; default = package.name
namespace  = "vtz"                # exported-target prefix; default = package.name

[targets.vtz]
type    = "static-library"
sources = ["src/*.cpp"]
includes = { public = ["include/api"], private = ["include/impl"] }
install = true                                    # this target is a product
public-headers = { base = "include/api" }         # REQUIRED to export a library
# full form:
# public-headers = [
#   { base = "include/api", patterns = ["**/*.h", "!**/detail/**"] },
# ]

[targets.vtz-tldr]
type    = "executable"
sources = ["etc/tldr.cpp"]
install = true                                    # → <prefix>/bin/vtz-tldr
```

### Semantics

- `install = true` (default **false**, all kinds) marks a product.
  Executables → `bin/`; static libraries → `lib/` + export in the Config;
  future `interface-library` (B10) → headers + INTERFACE export, no
  artifact.
- `public-headers`: one table or a list of tables.
  - `base` — **must be one of the target's `includes.public` dirs (or a
    `${gen}` public include dir from B4)**; anything else is an error,
    because the exported `INTERFACE_INCLUDE_DIRECTORIES` will claim
    `<prefix>/include` and the declarative reading must not lie about what
    is under it.
  - `patterns` — globs relative to `base`, `!`-negations allowed; default:
    the header-extension set `["**/*.h", "**/*.hpp", "**/*.hh",
    "**/*.hxx", "**/*.inc", "**/*.ipp"]`.
  - Files install to `include/<path-relative-to-base>`. Two entries (or two
    targets) mapping different content to the same `include/` path → hard
    error at install time.
  - A `static-library` with `install = true` but no `public-headers` is an
    error ("a library without headers cannot be consumed; declare
    public-headers or install = false") — catches the forgot-headers
    half-export.
- The exported manifest rewrites the target's public includes to the single
  `<prefix>/include` (that is what FILE_SET install does too); private
  includes vanish (they never propagated).
- `runtime-data`, `${install-prefix}`, closure rules: spine §0.3–0.5.

### Corpus use sites (before → after)

- **ninja** (§7 — before: build stops at `build/ninja`, no way to say ninja
  is the product and `ninja_test`/7 perftests are not):
  `[targets.ninja] install = true` — one line; `cpp-pkg install
  --prefix ~/.local` yields `bin/ninja`. Nothing else changes.
- **json-tui** (§8 — before: `install(TARGETS json-tui RUNTIME...)`
  dropped): `install = true` on the one executable.
- **cppcheck** (§3 — before: `stage-data.sh` + hardcoded
  `FILESDIR="/usr/local/share/Cppcheck"` define): §0.4 + §0.5 blocks
  verbatim; delete `stage-data.sh`; `install = true` on the binary. The
  build tree stops being silently broken on first `cpp-pkg build`.
- **vtz** (§8 — before: nothing attempted; upstream FILE_SET HEADERS
  BASE_DIRS `include/api`, EXPORT namespace `vtz::`,
  configure_package_config_file): the block above is the whole story;
  `public-headers = { base = "include/api" }` is the FILE_SET line;
  upstream's `etc/date_util_example` consumer then works via
  `find_package(vtz)` against the emitted Config.
- **googletest** (§5 — before: adopting CppPkg.toml would orphan the
  `find_package(GTest)` ecosystem): `[export] cmake-name = "GTest"
  namespace = "GTest"` + `install = true` and
  `public-headers = { base = "googletest/include" }` on the four libraries
  → `GTestConfig.cmake` with `GTest::gtest` / `GTest::gtest_main`, and
  mode (b) of the migration re-consumes it — closing the loop that today
  only works because upstream's CMake still exists.
- **benchmark** (§6): `install = true` + `public-headers = { base =
  "include" }` on `benchmark` and `benchmark_main`; ConfigVersion
  SameMajorVersion matches upstream; `.pc` files are the recorded loss
  (§0.6). Fixpoint test: probe the installed Config, diff against
  `cppkg-manifest.json`.
- **cpptrace** (§9): `install = true`, `public-headers = [{ base =
  "include" }, { base = "${gen}/include" }]` — the generated
  `version.hpp` (B4 tier 1) installs alongside; `libdwarf` becomes
  `find_dependency(libdwarf)` + a recorded pin instead of upstream's
  vendored `libdwarf.a` (deliberate, documented divergence — §0.3).
- **abseil** (before: "cul-de-sac artifact-wise"): with B9,
  `[target-defaults] install = true` + `public-headers = { base = ".",
  patterns = ["absl/**/*.h", "absl/**/*.inc"] }` once — matches upstream's
  `install(DIRECTORY absl FILES_MATCHING *.h *.inc)`; `[export] namespace =
  "absl"` gives `absl::strings` et al. 93 targets export without 93
  repetitions.

### Interactions

- **B1 flags:** public `cxx-flags` (json-tui's `-fno-exceptions`) land in
  the Component's `compile_options` → `INTERFACE_COMPILE_OPTIONS` in the
  shim — field already exists end-to-end. Consumers get our headers as
  `-isystem` via the B1c probe fix, symmetric with how we consume others.
- **B3 cfg:** an installed prefix is a *per-platform artifact* (as with
  CMake install trees); the exported manifest is the projection selected at
  build time. cfg sub-tables merging into `runtime-data`/`public-headers`
  lists compose with whichever S3 surface wins (they are ordinary list
  keys).
- **B4 codegen:** generated public headers export via `base =
  "${gen}/..."`; `${install-prefix}` shares B4's interpolation grammar and
  namespace — one `${...}` spec, jointly owned with the codegen design.
- **B2 tests:** `type = "test"` (or `test = true`) targets can never set
  `install = true` (error), and dev-deps in an exported closure error
  (§0.3).
- **B8:** `!` negation grammar reused in `patterns` here and in
  `runtime-data`.
- **B9:** `install` and `public-headers` are inheritable via
  `[target-defaults]` (abseil's 93×).
- **B10:** `interface-library` exports as INTERFACE imported target —
  `shim.rs` already emits those for extracted deps.
- **B7 hermeticity:** the emitted Config/manifest contain only
  prefix-relative paths and `requires` pins — no store paths, nothing
  machine-specific; the hermeticity scan gets one more rule (error if an
  absolute path survives into an exported manifest).

### Linux story

Layout, DESTDIR, and relative-path Configs are the FHS/distro contract;
nothing macOS-specific exists in the surface. Copy staging is plain
`std::fs`. Validation on keres (S5/S6): install ninja and cppcheck into a
scratch prefix and run *from the prefix* — the cppcheck run proves
`${install-prefix}` + `share/` staging on Linux (its cfg lookup is the
sharpest cross-platform test we have); install vtz and build upstream's
`date_util_example` against the emitted Config with gcc-16. Frameworks
never appear in Linux manifests; `-lrt`-class system libs pass through
`requires`/system-libs untouched. `.pc` deferral is the one real Linux
cost (§0.6).

### Implementation sketch

- `schema.rs`: `TargetSpec { install: bool, public_headers:
  Vec<HeaderSet>, runtime_data: Vec<DataSet> }`; `ExportMeta { cmake_name,
  namespace }` on `ProjectFile`; validation rules above.
- `graph.rs`: expand `runtime-data`/`public-headers` globs (shares the B8
  matcher); `${install-prefix}` substitution in define values; export
  closure check.
- `ninja_gen.rs`: `copy` rule + edges for build-tree runtime-data staging.
- `shim.rs`: `manifest_from_project(plan, export_meta) -> Manifest`;
  relative-path emission mode (`_IMPORT_PREFIX`); SameMajorVersion
  ConfigVersion variant; `@prefix@` serialization for
  `cppkg-manifest.json`.
- `cli.rs`: `Install` subcommand (build → stage → emit); `--prefix` on
  `Build` for the define.
- New integration test: install → probe → fixpoint diff (extends the
  existing `shim_roundtrip_cmake_properties` test to local targets).

### Costs

- Two declarations per exported library (`install` + `public-headers`) even
  when the answer is "obviously everything under my one public include
  dir" — the 90% case pays a line for the 10%'s precision.
- `public-headers.base` duplicates information already in
  `includes.public` for every wave-1 library except abseil; drift is
  prevented by the base-must-be-public-include rule, but the duplication
  is real and permanent.
- Explicitness scales linearly with target count; only B9 inheritance
  keeps abseil-class projects tolerable.

---

## Candidate B — "The public interface *is* the package" (derived headers, declared products)

Same spine, same `install = true` product marking, but **the header set is
derived from what the manifest already declares**: a library's public
include dirs are its public headers. `public-headers` exists only as an
override for the exceptional case.

### TOML surface

```toml
[targets.vtz]
type    = "static-library"
sources = ["src/*.cpp"]
includes = { public = ["include/api"], private = ["include/impl"] }
install = true
# no public-headers: everything header-shaped under include/api installs

[targets.weird]
type    = "static-library"
includes = { public = ["."] }          # abseil-style repo-root public include
install = true
public-headers = { base = ".", patterns = ["absl/**/*.h", "absl/**/*.inc"] }
# override: replaces derivation for this target (same shape as Candidate A)
```

### Semantics

- For each exported library, each `includes.public` dir `d` (including
  `${gen}` dirs): every file under `d` matching the default
  header-extension set (as in A) installs to `include/<rel-path>`.
  Symlinks are not followed; collisions (same `include/` path, different
  bytes) are hard errors.
- `public-headers` (identical shape and rules to Candidate A) **replaces**
  the derivation for that target when present. One knob, not two: the
  override is total, never merged, so the answer to "what installs?" is
  always exactly one of "derived" or "declared".
- Everything else — products, namespace, runtime-data, prefix define,
  closure, emission — is the spine, unchanged.
- Derivation edge cases pinned down:
  - a public include dir containing *no* header-extension files → install
    error naming the dir ("public include dir installs nothing; declare
    public-headers or fix includes") — catches the broken-export shape.
  - non-header files under a public include dir are silently *not*
    installed (extension filter) — matching CMake `install(DIRECTORY ...
    FILES_MATCHING)` practice, and exactly what abseil upstream does.
  - overlapping public dirs across exported targets dedupe when byte-equal
    (the common shared-`include/` layout), error otherwise.

### Corpus use sites

Identical to Candidate A **except**:

- **vtz**: drops the `public-headers` line entirely — `includes = { public
  = ["include/api"] }` already says it; the FILE_SET BASE_DIRS ↔
  `includes.public` correspondence becomes exact rather than restated.
- **benchmark, googletest, cpptrace**: drop their `public-headers` lines
  (cpptrace's `${gen}/include` public include dir is derived too — the
  generated `version.hpp` installs with zero extra words).
- **abseil**: keeps the override (repo-root public include dir needs the
  `absl/**` scoping); one override in `[target-defaults]`, same as A.
- **ninja, json-tui, cppcheck**: identical to A (executables have no
  headers to derive).

Net across wave 1: the override appears exactly once (abseil); every other
library's install block is `install = true` and nothing else.

### Interactions

As Candidate A, plus one: **derivation makes the B1c `-isystem` and B3 cfg
stories self-consistent by construction** — whatever a cfg projection put
into `includes.public` is what installs, so a Linux install of a
cfg-conditional manifest cannot desynchronize its header set from its
include claims. In Candidate A the same is enforced by the base-check rule
rather than by construction.

### Linux story

Identical to A. Derivation is pure path/extension logic — no platform
surface at all.

### Implementation sketch

Candidate A's sketch plus ~40 lines in `graph.rs`: derive `HeaderSet`s from
`includes.public` when `public_headers` is empty; the empty-derivation and
collision errors. Strictly a superset of A's machinery — an A→B or B→A
pivot later is cheap, which lowers the risk of picking either.

### Costs

- Derivation is implicit behavior: `install = true` sweeps files never
  individually named. Mitigations: the extension filter, the
  empty-derivation error, and `cpp-pkg install --list` (print the staging
  plan without writing — cheap, worth shipping with either candidate). The
  residual risk is a stray `scratch.h` in a public include dir being
  published; Candidate A has the same risk within `base` and the same
  mitigation (default patterns), so the *marginal* risk is small.
- Two ways to say headers (derived + override) — but the override is
  total-replacement, and wave-1 needs it exactly once.
- "Public include dir" and "publishable header tree" are conflated; a
  project whose public dir deliberately mixes installable and
  non-installable headers of the *same extension* must use the override.
  No wave-1 project has this shape.

---

## Candidate C — "Package-level `[install]` projection table"

Install is a section, not a target attribute: one table describes the
products, CMake-`install()`-style. (Runtime-data **stays on targets** even
here — §0.4's build-tree staging is a target concern, and cppcheck is
broken without it; only product/header selection moves.)

### TOML surface

```toml
[install]
targets = ["vtz", "vtz-tldr"]          # kind decides bin/ vs lib/+export
namespace = "vtz"                       # export namespace; default package.name
cmake-name = "vtz"                      # default package.name

[install.headers]
"include" = [{ base = "include/api" }]  # dest-dir = list of header sets
```

### Semantics

- `targets` — the product list; names must exist; kinds route them.
  Referencing a test target is an error. Closure rule (§0.3) applies: a
  non-listed local target in a listed target's closure errors.
- `[install.headers]` — keys are destinations under the prefix, values are
  header-set tables as in Candidate A. Not attached to any target: the
  exported manifest maps *every* exported library's public includes to
  `<prefix>/include` and trusts this table to have populated it. That
  trust is checked weakly (warning when an exported target has a public
  include dir no header set covers) — it cannot be checked strongly,
  because the table is not per-target.
- Everything else is the spine.

### Corpus use sites

- **ninja / json-tui**: `[install] targets = ["ninja"]` — as terse as A/B.
- **cppcheck**: `[install] targets = ["cppcheck"]` + target-level
  runtime-data (§0.4) — the split surface shows immediately: install
  config lives in two places.
- **vtz / benchmark / googletest / cpptrace**: one `[install]` block each;
  googletest reads nicely (`cmake-name = "GTest"` sits beside the target
  list it renames).
- **abseil**: `targets = [ ...93 names... ]` — regenerated by the
  generator, reviewable by nobody. B9 cannot help (defaults are per-target;
  this list is not). This is the candidate's failure case.

### Interactions

As A, minus the B9 synergy (the products list cannot be inherited) and
minus A/B's per-target header-to-include coherence check (weak warning
only). cfg-conditionality of the products list (a target that exists only
on Windows) would need cfg support on `[install]` too — a new cfg site the
other candidates get for free by living on targets.

### Linux story

Identical spine. No candidate-specific delta.

### Implementation sketch

As A, with `InstallMeta` on `ProjectFile` instead of target fields; the
weak header-coverage warning replaces A's base-check.

### Costs

- Splits the description of a target across two places; the
  headers-vs-includes coherence guarantee degrades from error to warning —
  the declarative reading *can* now lie (Config claims `<prefix>/include`
  serves a target whose headers were never listed).
- Scales worst of the three (abseil), and uniquely resists B9.
- Its one genuine virtue — everything about publishing in one visible
  block — is mostly recoverable in A/B via `cpp-pkg install --list`.

---

## Recommendation

**Candidate B.** It satisfies all four tie-breakers most cleanly: the
declarative reading cannot lie (headers *are* the public includes, enforced
by construction); simple projects stay simple (a library exports with one
added line, an app with one); it is one orthogonal primitive (`install`
marks products; the existing `includes.public` already carries the
interface) rather than a parallel header-declaration vocabulary; and the
shapes are existing schema conventions (target keys, kebab-case, the same
table forms as A when overriding). Cargo familiarity: `install = true` reads
like `publish`/bin-vs-lib intuitions, and "your declared public interface is
what ships" is precisely Cargo's model. A is the safe runner-up (one
explicit line more per library, marginally better greppability); the
implementation delta between them is ~40 lines, so this is a genuinely
reversible taste call. C fails tie-breaker (1) and abseil.

---

## B8 — glob exclusion (recommendation, engineering-dominated)

Adopt `!`-prefixed negative patterns inside the existing `sources` arrays —
`sources = ["cli/*.cpp", "!cli/main.cpp"]` — rather than a separate
`exclude` key: it keeps one list (ordering-independent semantics: union of
positives minus union of negatives, applied after expansion, before the
existing lexicographic sort), it needs no new table shape, and the same
grammar is then reused verbatim by `runtime-data.patterns` and
`public-headers.patterns` above (one exclusion syntax across the whole
schema, tie-breaker 3). Semantics: a negative pattern that matches nothing
is a **warning**, not an error (upstream deleting `benchmark_main.cc` should
not break the manifest; the warning still surfaces drift), while a manifest
whose *positives* expand to nothing keeps the existing hard-error behavior;
a bare list of only negatives is a schema error. This directly rewrites the
three corpus workarounds — cppcheck's unreproducible `cli` library
(`["cli/*.cpp", "!cli/main.cpp"]` — and the testrunner can finally share
it), benchmark's 19 hand-listed files (`["src/*.cc",
"!src/benchmark_main.cc"]`), vtz's 7-file bench list — and ninja's GAPS
correctly notes its win32 split is B3's problem, not this. Implementation:
~30 lines in `graph.rs` glob expansion plus schema validation; no
interaction with hashing, profiles, or the store.

## B9 — target-defaults (recommendation, engineering-dominated)

Adopt a single `[target-defaults]` table accepting the target-scoped subset
of keys (`cxx-std`, `c-std`, `defines`, `includes`, `dependencies` is
**excluded** — inherited dep edges would make the graph unreadable — plus
`cxx-flags`/`link-flags` when B1 lands and `install`/`public-headers`/
`runtime-data` when B6 lands), with merge semantics fixed as: scalars —
target value overrides default; list/visibility keys — default entries
prepend, target entries append (so a target can extend but the manifest
reader sees defaults applied uniformly); no per-key opt-out in v1 (a target
needing to *subtract* a default define states the full story locally by
overriding — if wave 2 demands subtraction, revisit). This kills the
measured repetition directly: abseil's 29% (two identical lines × 93
targets), cppcheck's 4-defines+`cxx-std` × 5 ("single ugliest thing"),
googletest's `cxx-std = 17` × 14 with its silent toolchain-default hazard,
benchmark and json-tui's directory-scoped defines. Explicitly deferred, per
BACKLOG: target templating (ninja's 7 perftests) and `targets-from` file
inclusion — defaults alone fix everything measured, and both deferrals are
purely additive later. Implementation: merge at `schema.rs` load time
(before validation, so errors point at effective values); `cpp-pkg build
--query` already shows the resulting compile commands for verification.

---

## OPEN QUESTIONS for the taste judge

- **Q1 (the pick):** Candidate A (declared headers) vs B (derived with
  total-override) — B recommended; is implicit derivation acceptable under
  "declarative reading never lies", given the empty-derivation error and
  `install --list`? (C is not recommended; overrule only with a reason
  abseil doesn't refute.)
- **Q2:** Interpolation spelling `${install-prefix}` — confirm it joins
  B4's `${...}` namespace (`package.*`, `pin.*`, `gen`), and confirm
  defines-values-only scope for v1.
- **Q3:** Executables in the *export* (IMPORTED executables for
  protoc-style tool packages) — defer to wave 2, or reserve the shape now
  (`install = true` on an executable additionally exporting it when a
  namespace is configured)?
- **Q4:** `[export]` table vs flat `[package]` keys
  (`export-namespace = ...`) — the candidates use a table (two related
  keys, tables-over-flags); confirm.
- **Q5:** Should `install = true` be defaultable via `[target-defaults]`
  only, or also implied for a package with exactly one executable target
  (json-tui/ninja read "obviously the product")? Recommendation: no
  implication — one explicit line, simple stays honest.
- **Q6:** pkg-config `.pc` emission — deferred in §0.6, but it is the main
  Linux-ecosystem consumption path for gtest/benchmark-class packages.
  Ride along in v1 (small: same manifest, text template) or hold for
  wave-2 evidence?
- **Q7:** A `prefix`-form dependency (`foo = { prefix = "/opt/foo" }`)
  reading `cppkg-manifest.json` directly — natural completion of the
  round-trip (consume what `cpp-pkg install` produced without a probe),
  but a new dependency source form with store/hashing questions. In scope
  for this design or split out?
- **Q8:** Static-closure vendoring (cpptrace upstream bundles
  `libdwarf.a`): confirm the defer + `requires`-pins stance, or demand a
  `bundle = true` escape now?
- **Q9:** ConfigVersion policy: SameMajorVersion when `version` is
  present, error for versionless library exports — confirm, or prefer
  accept-any (v0 shim behavior) until a solver exists?
