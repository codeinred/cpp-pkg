# `CppPkg.toml` — schema (v0 + wave 1)

Status: **normative, post-wave-1** (2026-08-14). The v0 core is
oracle-reviewed (2026-08-13; concrete choices and verdicts in
`DESIGN_CHOICES.md`). Wave-1 additions — marked **(since wave 1)** in their
headings — were decided in the S2 design round; their binding spec is
`docs/design/wave1-extensions.md`, and this document is its user-facing
fold-in. Key convention: kebab-case for all TOML keys (`exposes-namespace`,
`cxx-std`).

One language, three cross-cutting rules worth knowing before the example:

- **One value grammar.** `includes`, `defines`, `dependencies`, and the
  target flag keys all share one shape: a bare list is all-private sugar;
  the full form is `{ public = [...], private = [...] }`.
- **One conditional spelling.** A `cfg.<predicate>` sub-table nests inside
  the scope it conditions (`[targets.x.cfg.linux]`, `[flags.cfg.clang]`);
  collections keyed by user-chosen names are conditioned at package scope
  (`[cfg.windows.dependencies.winreg]`). There is no inline `when = "..."`
  form — it is rejected, not reserved.
- **One interpolation grammar.** `${...}` with a closed vocabulary, legal
  only in whitelisted positions (table below). Unknown variables and `${`
  outside those positions are hard errors; `$${` escapes a literal `${`.

## Annotated example

```toml
schema-version = 1                  # required; format versioning from day one

[package]
name = "myapp"                      # required; charset [a-zA-Z0-9_-]+
version = "0.1.0"                   # optional — but required to export
                                    # (install) a library (since wave 1)

[export]                            # (since wave 1) optional; defaults shown
cmake-name = "myapp"                # default = package.name; find_package name
namespace  = "myapp"                # default = package.name; IMPORTED prefix

# ------------------------------------------------------------------
# Toolchain presets (optional). Selected via `cpp-pkg build --toolchain
# <name>`; a path argument (`--toolchain /usr/bin/clang++`) also works. With
# neither, CppPkg auto-detects `c++` on PATH.
[toolchains.gcc-homebrew]
cxx = "g++-16"
cc  = "gcc-16"                      # optional; derived from cxx if omitted
ar  = "gcc-ar-16"                   # optional; detected if omitted
# (target/sysroot/stdlib fields are future additive extensions; toolchain
# *identity* always comes from detection output, never from the preset name)

# ------------------------------------------------------------------
# Profiles: named build flavors. The four built-ins, named after the CMake
# configs: debug | release | relwithdebinfo | minsizerel (selected via
# `cpp-pkg build --config debug`; default release). `base-config` stays
# RESERVED for future custom profiles.
[profiles.debug]
cxx-flags  = ["-fsanitize=address"] # consumer targets only — see Semantics
c-flags    = []                     # routed to the C driver only
link-flags = ["-fsanitize=address"]

# ------------------------------------------------------------------
# (since wave 1) Package flags: every target, every profile. This is
# hoisted-profile semantics — an environment statement, so there is no
# public/private split here. Non-ABI entries never reach dependency builds;
# ABI-classified entries do, and fold into dependency config hashes (same
# machinery profiles already use).
[flags]
cxx-flags  = ["-Wall", "-Wextra"]
c-flags    = []
link-flags = []

[flags.cfg.clang]                   # conditional refinement — see cfg below
cxx-flags = ["-Wthread-safety"]

# ------------------------------------------------------------------
# Dependencies: the FULL transitive closure, declared by the user.
# Keys: charset [a-zA-Z0-9_-]+ ("::" and "/" thereby unavailable, reserving
# qualified-reference syntax). Consumers reference the *targets* a package
# exports, not the package key.
[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.2.0" }

# (TOML forbids wrapping inline tables across lines; use a standard table for
# dependencies with more than a couple of fields.)
[dependencies.spdlog]
git     = "https://github.com/gabime/spdlog"
tag     = "v1.15.3"
options = { SPDLOG_FMT_EXTERNAL = "ON" }
needs   = ["fmt"]                   # find_dependency edge — see Semantics

[dependencies.zlib]
url    = "https://zlib.net/zlib-1.3.1.tar.gz"
sha256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"

[dependencies.absl]                 # (since wave 1) patched dependency
git     = "https://github.com/abseil/abseil-cpp"
rev     = "255c84dadd029fd8ad25c5efb5933e47beaa00c7"
patches = ["patches/absl-0001-mark-testing-lib-TESTONLY.patch"]

[dependencies.zstd-src]             # (since wave 1) configure in a subdir
git    = "https://github.com/facebook/zstd"
tag    = "v1.5.7"
subdir = "build/cmake"              # CMakeLists.txt lives below the root

[dependencies.zstd]                 # (since wave 1) system dependency:
system      = true                  # resolve from the machine, never build
min-version = "1.5"                 # optional

# Source forms (exactly one per dependency):
#   git + tag | git + rev (commit)   — tag resolved to a commit, pinned in
#                                      CppPkg.lock; git submodules are an
#                                      ERROR (unsupported, not ignored)
#   url + sha256                     — tarball/zip (.tar.gz/.tgz/.zip, and
#                                      since wave 1 .tar.xz/.tar.bz2)
#   system = true                    — (since wave 1) see System dependencies
# Common fields:
#   options = { KEY = "VALUE" }      — CMake cache options for the dep build.
#                                      Hashed as LITERAL strings: ON/TRUE/1
#                                      are distinct hash inputs by design;
#                                      never "normalize" them (it would
#                                      invalidate every store entry).
#   needs   = ["depkey", ...]        — packages whose config files this dep's
#                                      config requires (find_dependency)
#   find-package = "GTest"           — find_package() name when it differs
#                                      from the dep key (probe uses this)
#   exposes-namespace = ["fmt"]      — claim ownership of all targets whose
#                                      namespace is `fmt::` (when the dep key
#                                      doesn't match the exported namespace)
#   exposes-targets   = ["fmt::fmt"] — claim explicit targets; the mapping
#                                      form renames: { "fmt::fmt" = "fmt" }
#   patches = ["patches/x.patch"]    — (since wave 1) applied in order after
#                                      fetch, before configure; see Patches
#   subdir  = "build/cmake"          — (since wave 1) configure-root subdir
#   system-includes = false          — (since wave 1) opt this dep's headers
#                                      out of -isystem (back to -I)

# ------------------------------------------------------------------
# (since wave 1) Dev-dependencies: identical grammar, one shared resolution
# namespace with [dependencies] (key collision across the tables is an
# error). Reachable only from dev/test targets; never exported; locked
# eagerly, fetched/built lazily.
[dev-dependencies]
googletest = { git = "https://github.com/google/googletest", tag = "v1.17.0", find-package = "GTest" }

# ------------------------------------------------------------------
# (since wave 1) Conditional dependency presence: package-scope cfg tables
# (a magic `cfg` key can't live inside a user-named collection).
[cfg.windows.dependencies.winreg]
git = "https://github.com/example/winreg"
tag = "v1.0"

# ------------------------------------------------------------------
# (since wave 1) Codegen. Two tiers + a checked-in mode. Outputs land under
# the generated root `${gen}` (= build/gen/); source-tree writes are
# refused by construction (the one exception is `cpp-pkg gen` refreshing a
# `checked-in` path).
[generate.version-header]           # tier a — template substitution
template = "src/version.hpp.in"     # @VAR@ substitution, @ONLY semantics
output   = "src/version.hpp"        # lands at ${gen}/src/version.hpp
vars     = { PROJECT_VERSION = "${package.version}" }

[generate.browse-py-h]              # tier b — declared command (argv, no
command = ["sh", "src/inline.sh", "kBrowsePy"]   # shell), sandboxed edge
stdin   = "src/browse.py"           # auto-input
stdout  = "build/browse_py.h"       # auto-output, under ${gen}
inputs  = ["src/inline.sh"]

[generate.lexer]                    # checked-in mode: build compiles the
command    = ["re2c", "-o", "lexer.cc", "src/lexer.in.cc"]  # committed file;
stdout     = "lexer.cc"             # `cpp-pkg gen` regenerates + copies over
inputs     = ["src/lexer.in.cc"]    # the checked-in path; `gen --check`
checked-in = "src/lexer.cc"         # byte-diffs (CI drift guard)

# ------------------------------------------------------------------
# (since wave 1) Target defaults: filled into every eligible target before
# validation. Scalars fill-if-absent (target wins); lists/visibility keys
# PREPEND (defaults stay first, targets append).
[target-defaults]
cxx-std = 20
defines = { private = ["MYAPP_INTERNAL"] }
install = true                      # skips dev/test targets automatically

# ------------------------------------------------------------------
# Targets. Table key = target name; charset [a-zA-Z0-9_-]+ (no "::", so local
# names can never collide with dependency-exported names like "fmt::fmt").
[targets.core]
type    = "static-library"
sources = ["src/core/**/*.cpp",     # globs resolve in sorted (lexicographic
           "!src/core/main.cpp"]    # byte-order) form at generation time;
                                    # (since wave 1) "!" patterns subtract —
                                    # see Glob exclusion
cxx-std = 20                        # strict -std=c++20; `cxx-extensions =
                                    # true` (gnu++20) is reserved, default
                                    # false
includes = { public = ["include"], private = ["src", "${gen}/src"] }
defines  = { public = ["CORE_API="], private = ["CORE_INTERNAL"] }
dependencies = { public = ["fmt::fmt"], private = ["spdlog::spdlog"] }
cxx-flags = { private = ["-Werror"],          # (since wave 1) per-target
              public  = ["-fno-exceptions"] } # flags — see Flags
install = true                      # (since wave 1) export this target;
                                    # public headers are DERIVED from
                                    # includes.public — see Install & export

[targets.core.cfg.linux]            # (since wave 1) platform conditionals:
sources = ["src/core/epoll.cpp"]    # additive-append onto the same keys
defines = { private = ["USE_EPOLL"] }

[targets.myapp]
type    = "executable"
sources = ["src/main.cpp"]
dependencies = ["core", "Threads::Threads"]  # bare list == all-private
                                    # (sugar; applies uniformly to includes/
                                    # defines/deps/flags). Threads::Threads
                                    # is a builtin — no declaration needed
                                    # (since wave 1)
install = true                      # → <prefix>/bin/myapp

[targets.myapp.run]                 # nothing here — run entries are for
                                    # tests; shown below

# --- testing (since wave 1) ---------------------------------------
[targets.tests]
type = "executable"
test = true                         # implies dev = true; runner-registered
sources = ["test/*.cpp"]
dependencies = ["core", "GTest::gtest_main"]

[targets.bench]
type = "executable"
dev  = true                         # dev graph, deliberately NOT a test

[[targets.tests.run]]               # N entries = N invocations; zero
name           = "bad-env"          # entries = one default invocation
args           = ["--fast"]
cwd            = "test-fixtures"    # relative to project root
env            = { MYAPP_DATA = "${gen}/data" }
env-remove     = ["TZ"]
expect-failure = true               # pass iff nonzero exit or signal death

# --- runtime data (since wave 1) ----------------------------------
[targets.cppcheck-like]
type = "executable"
runtime-data = [
  { from = "cfg",       patterns = ["*.cfg"] },
  { from = "platforms", patterns = ["*.xml", "!*-unsigned.xml"] },
  { from = "addons" },              # whole dir; default to = last component
]
defines = { private = ['FILESDIR="${install-prefix}/share/myapp"'] }
```

## Semantics

### Target references and the naming ladder

A `dependencies` entry is either a sibling target name (`core`) or a
dependency-exported target name (`fmt::fmt`). Resolution follows the ladder
(per `CPP_PKG.md`):

0. **(since wave 1)** Builtin pseudo-package names resolve first and cannot
   be shadowed. The v1 builtin list is exactly `Threads::Threads` (see
   Builtins below). `exposes-namespace`/`exposes-targets` claiming a builtin
   is a hard error whose message is the fix ("builtin pseudo-package; delete
   this line").
1. If the name is unique across all dependencies' manifests, it resolves
   directly.
2. Otherwise, a name beginning with `<depkey>::` belongs to the dependency
   whose key is `<depkey>`.
3. Otherwise, `exposes-namespace` / `exposes-targets` declarations decide
   ownership (`exposes-targets` in mapping form also renames).
4. A reference that remains ambiguous is a **hard error at resolve time** —
   never first-wins — and the error lists every candidate owning package with
   the `exposes-*` addition that would disambiguate.

These declarations are also the user-facing override for the extractor's
namespace-attribution heuristic (transitive `find_dependency` targets appear
in more than one probe's output; attribution must be overridable).

Entries in a `dependencies` array are **string-or-table** from v1 of the
schema: strings are sugar, and the table form (`{ target = "...", ... }`) is
reserved for per-edge attributes (renames, link-only) without a breaking
change. Only strings are implemented today.

### Visibility and propagation

`public` entries of a library's `includes`/`defines`/`dependencies` (and,
since wave 1, `cxx-flags`/`c-flags`/`link-flags`) propagate to its
consumers; `private` do not — with one crucial exception: **for a
`static-library`, `private` dependencies propagate as link-only edges**
(CMake's `$<LINK_ONLY:...>` behavior). A static library does not link, so its
private deps' *artifacts* must still reach the final link closure; only their
compile requirements stop. (`myapp → core → private spdlog`: myapp links
spdlog but sees none of its headers/defines.) The manifest IR's separate
compile-edge/link-edge fields carry this directly.

An `interface` visibility bucket and `interface-library` target kind remain
deferred (purely additive later); header-only *dependencies* still work via
extraction (INTERFACE imported targets).

### Flags (since wave 1)

Three surfaces, one grammar (flags are **argv words, not shell text** —
two-word flags are two entries):

- **`[flags]`** — every target, every profile. Hoisted-profile semantics:
  consumer-targets-only, *except* ABI-classified entries, which inject into
  dependency builds and fold into each dependency's config hash via the
  existing profile-ABI machinery. No public/private split at this scope (it
  is environment, not interface). `[flags.cfg.<pred>]` refines it
  conditionally.
- **`[profiles.<p>]`** — unchanged from v0 (per-flavor).
- **Per-target `cxx-flags` / `c-flags` / `link-flags`** — the standard
  visibility-split value. Language routing as in profiles.

**The propagation fence.** Public-bucket entries are classified at manifest
load; private is fully open, and unknown flags fail open (allowed). Exactly
four classes are rejected from the public bucket:

| class | members | why |
|---|---|---|
| ABI | `-D_GLIBCXX_*`, `-stdlib=*`, `-f*abi*`, … | error at **any** target scope: ABI belongs to `[flags]`/profiles, where it reaches dep builds and their hashes |
| sanitizer | `-fsanitize*` | error |
| warning | `-W…` (except `-Wl,`/`-Wa,`/`-Wp,` transports), `-w` | "warnings are private by nature; a library cannot volunteer its consumers into a diagnostic policy" |
| opt/debug | `-O*`, `-g`, `-g[0-9]`, `-ggdb*`, `-glldb*` | "optimization level is the consumer's (profile's) decision" |

Classification unwraps driver pass-throughs before matching: `-Wl,`/`-Wa,`/
`-Wp,` payloads (comma-split) and the two-argv `-Xlinker`/`-Xpreprocessor`/
`-Xassembler` forms are classified individually — `-Wp,-D_GLIBCXX_DEBUG` is
caught by the ABI table; `-Wl,-framework,X` passes. `link-flags` public
buckets check only ABI/sanitizer. Public flags on an `executable` are an
error ("nothing can consume an executable").

`-D*`/`-U*`/`-I*`/`-isystem`/`-std=*` inside flag lists draw a **warning**
naming the dedicated key (`defines`/`includes`/`cxx-std`) — not an error
(`-UNDEBUG` has no schema home and is legitimate).

**Layering is last-wins, and a documented contract.** Compile line for a TU
of target `T`, left to right:

1. toolchain/driver defaults + the selected profile's built-in config flags;
2. the ABI injection set (ABI-classified `[flags]` + profile entries);
3. `[flags]` (non-ABI remainder), then its matching `cfg` groups;
4. `[profiles.<selected>]` flags;
5. public flags propagated from `T`'s transitive compile-visible
   dependencies (topological order, deduplicated by contributing target —
   diamonds contribute once);
6. `T`'s own public flags (unconditional, then matching cfg groups);
7. `T`'s own private flags (same order) — most specific voice last.

Link line: `T`'s objects → `[flags].link-flags` → profile `link-flags` →
`T`'s own `link-flags` → the link closure **interleaved**: each member's
library input immediately followed by that member's own `link-flags` (so raw
`-lrt`-class words land after the archive that references them — required
under GNU ld `--as-needed`). `link-flags` on a static library propagate
link-only; consequence: public≡private for static-library `link-flags`
until a shared-library kind exists.

**`system-includes`** — default `true` on dependencies (their public
headers arrive `-isystem`: diagnostics suppressed, searched after all `-I`
dirs), `false` on project targets. Set `system-includes = true` on a
vendored project target to give *its consumers* `-isystem`; set
`system-includes = false` on a dependency to opt back into `-I`. Distinct
from `system = true` (a *system dependency* — see below).

Hashing: target flags and non-ABI `[flags]` entries never touch dependency
config hashes; ABI-classified `[flags]` entries fold in via the existing
profile-ABI rule.

### Platform conditionals: `cfg` (since wave 1)

Predicate vocabulary (closed): os — `windows`, `macos`, `linux`; family —
`unix` (true iff macos or linux); compiler — `clang` (**matches
AppleClang**), `gcc`, `msvc`. Truth comes from toolchain identity (what you
are building *for*), evaluated at plan time. Unknown atoms are hard errors
listing the vocabulary; combinators (`all(...)`, `any(...)`, `not(...)`,
version comparisons) and `apple-clang` are **reserved** with a distinct
error — the blessed future spelling is the quoted key
(`[targets.x.cfg."all(linux, gcc)"]`).

Placement (the one rule): a `cfg.<pred>` sub-table nests inside the scope it
conditions —

| Conditioned scope | Spelling |
|---|---|
| a target | `[targets.<t>.cfg.<pred>]` |
| the package flags layer | `[flags.cfg.<pred>]` |
| dependency presence | `[cfg.<pred>.dependencies.<key>]` |
| dev-dependency presence | `[cfg.<pred>.dev-dependencies.<key>]` |

Reserved (distinct error): `[target-defaults.cfg.*]`, `[cfg.*.targets.*]`,
`[cfg.*.generate.*]`, `[profiles.*.cfg.*]`. Inline `when = "..."` is
rejected outright.

Semantics:

- **Additive-append merge**, list-valued keys only: unconditional entries
  first, then matching groups in document order, per key *and* visibility
  bucket. Conditionable keys: `sources`, `includes`, `defines`,
  `dependencies` (target scope), `cxx-flags`, `c-flags`, `link-flags`,
  `runtime-data`. Scalars in a cfg group are a hard error ("conditional
  scalar overrides are not in v1"). `public-headers` is not conditionable
  (it is a total override; condition `includes.public` instead — header
  derivation follows for free). `dev`/`test` markers are not conditionable.
- Non-matching groups are validated (vocabulary, key rules) but never
  expanded — their globs stay unexpanded, their paths unchecked. `windows`
  atoms are accepted-but-false on the current platforms.
- **All declared deps are locked, always** (a windows-only dep locks from a
  Mac); only active-predicate deps are fetched/built/probed. Referencing a
  cfg'd-out dep's target from an active target is an unresolved-reference
  error naming the false predicate. Declaring one dep key on two branches
  (or branch + unconditional) is a hard error — bundled-on-one-platform /
  system-on-another is deliberately inexpressible in v1.
- Nested cfg = error; empty group = lint warning; zero sources after
  evaluation = the existing error, extended to name non-matching groups.
- No config-hash impact — cfg is a project-manifest concern.

Documentation convention: a transcribed upstream probe answer lives under
the narrowest true predicate with a comment `# transcribed: <upstream
check>` naming the check. The prefix is stable (lintable; mechanically
migratable when real probes land).

### Testing (since wave 1)

Two orthogonal markers plus a runner:

- **`dev = true`** (any target kind): dev-graph membership — may reference
  dev-dep targets and other dev targets; excluded from the default build and
  from export; buildable by explicit name.
- **`test = true`** (executables only): implies `dev`; registers with the
  runner. `test = true, dev = false` is a reserved error; `test` on a
  library errors with the hint "libraries use `dev = true`".
- The whole edge model: **a non-dev target may not depend on a dev target or
  a dev-dep-owned target; every other direction is legal.** Violations name
  the offending edge and the fix.
- **`[dev-dependencies]`**: same `DependencySpec` grammar, one shared
  namespace with `[dependencies]` (key collision = load error); a regular
  dep's `needs` may not name a dev-dep; `patches` and `system = true` are
  legal with identical semantics. Locking is **eager** (CppPkg.lock is the
  complete declared universe — platform- and dev-independent); fetch/build/
  probe are **lazy** (`cpp-pkg build` of a library does no store work for
  its dev-deps, and with a committed lockfile, no network either).
- **`[[targets.<t>.run]]`** (legal only when `test = true`): fields `name`
  (optional, unique per target), `args`, `cwd`, `env`, `env-remove`,
  `expect-failure` (default false). Zero entries = one default invocation.
  N entries = N invocations. Unknown fields are errors.
  - cwd: relative to project root; auto-created iff it resolves inside the
    top-level `build/` tree, otherwise it must exist (a missing fixture cwd
    fails that invocation legibly; the suite continues).
  - env: inherit → apply `env-remove` → apply `env`. Setting `""` and
    removing are distinct.
  - Pass criterion: exit 0 ⇒ pass. `expect-failure = true`: pass iff
    nonzero exit **or signal death** (`expect-signal` is reserved).
- **Runner**: `cpp-pkg test [FILTER...] [--config] [--toolchain] [--jobs N]
  [--list] [--verbose] [-- PASSTHROUGH...]` — builds the selected test
  targets through the ordinary pipeline, then spawns each invocation
  directly (argv array, **no shell**), serial by default (`--jobs` opt-in),
  captured output replayed on failure, `--` passthrough appended to every
  selected invocation. A FILTER matching nothing is a hard error; a project
  with no tests prints "no test targets" and exits 0.
- **Default build set**: `cpp-pkg build` = all non-dev targets. Unmarked
  manifests behave byte-identically to v0.

No hashing or lockfile impact: dev-deps hash, build, and cache exactly like
regular deps.

### Codegen: `[generate]` (since wave 1)

Named steps, exactly one of `template`/`command` each; step names share the
target charset. No ordering fields — a `${gen}` input that is another step's
output creates the edge implicitly; cycles are plan errors.

- **`${gen}` = `build/gen/`**, the single generated-output root. `output`/
  `stdout` paths are relative; `..` or absolute paths are hard errors —
  source-tree writes are refused by construction. Output collisions are
  plan-time errors, **case-insensitively on all platforms**.
- **Templates** (tier a): `@VAR@` substitution only (`@ONLY` parity).
  Unbound token = hard error naming the template line (never a silent
  empty); unused var = warning; `#cmakedefine`/`#cmakedefine01` = hard error
  ("not supported in v1").
- **Commands** (tier b): argv, no shell, run via a hidden sandboxed wrapper:
  cwd = project root, **no network** (macOS `sandbox-exec` / Linux
  `unshare -n`, both best-effort — where sandboxing is unavailable the step
  runs un-sandboxed with one warning per invocation naming the
  degradation). Declared outputs verified after exit 0; temp-write + atomic
  commit; mtime preserved on unchanged bytes (restat-friendly). Undeclared
  reads are undetected and host tool identity is unhashed — the same holes
  CMake has, documented.
- **Consumption**: generated headers join targets via `${gen}` entries in
  `includes` (always `-I`, never `-isystem` — your generated code is your
  code); a generated *source* must byte-match a declared output (no globbing
  under `${gen}`).
- **Laziness**: a step runs only when the requested build/test set
  references `${gen}`. Missing declared inputs are plan-time hard errors
  *for activated steps only* — a codegen-free build of a fresh clone never
  trips over a dormant step's inputs.
- **`checked-in` mode**: the blessed committed-generated pattern. The step
  lives outside the build graph — `cpp-pkg build` compiles the committed
  file with no questions asked; **`cpp-pkg gen`** regenerates and copies
  over the checked-in path (the one sanctioned source-tree write, behind an
  explicit verb); **`cpp-pkg gen --check`** byte-diffs (CI mode).
- No hash impact (deps build via CMake; `[generate]` never executes in the
  store).

### Interpolation: `${...}` (since wave 1)

Closed vocabulary, whitelisted positions; unknown variable = hard error
naming the vocabulary; `${` anywhere else = hard error; `$${` escapes.

| Variable | Value | Positions |
|---|---|---|
| `${package.name}` | `[package].name` | defines values; `[generate.*]` vars/argv |
| `${package.version}` (+ `.major/.minor/.patch`) | `[package].version` (error if unset; component error if non-integer) | same |
| `${pin.<depkey>.commit}` | resolved commit from CppPkg.lock (**base** commit — never the patch-composed id) | defines values; `[generate.*]` vars/argv |
| `${pin.<depkey>.requested}` | human ref (`v1.9.5` for `tag:`, sha for `rev:`) | same |
| `${gen}` | `build/gen/` | `sources`/`includes` entries; `[generate.*]` argv; run-entry `args`/`cwd`/`env` values |
| `${project-root}`, `${build-dir}` | absolute paths | run-entry `args`/`cwd`/`env` values |
| `${install-prefix}` | `install --prefix` value; default `/usr/local` (overridable via `build --prefix`) | defines values only |
| `${pin.self.*}` | **reserved** (root build will be a hard error) | — |

Interpolated defines are first-class: version stamps are a define, not a
codegen step (`defines = { private = ['VERSION="v${package.version}"'] }`).
`${install-prefix}` in a define rebuilds exactly the TUs embedding it —
dependency store keys are untouched.

### Glob exclusion (since wave 1)

`sources`, `runtime-data.patterns`, and `public-headers.patterns` accept
`!`-prefixed negative patterns. Order-independent semantics: union of
positives minus union of negatives, applied after expansion, before the
lexicographic sort. A negative matching nothing is a **warning** (surface
upstream drift, don't break); positives expanding to nothing stay the
existing hard error; a list of only negatives is a schema error.

### Target defaults (since wave 1)

`[target-defaults]` fills every eligible target before validation (errors
point at effective values; `cpp-pkg build --query` shows results).

- Accepted keys: `cxx-std`, `c-std`, `defines`, `includes`,
  `system-includes`, `install`, `public-headers`, `runtime-data`.
- Excluded: `dependencies`, `dev`/`test`/`run`, `sources`, `type`. The flag
  keys are **reserved** with an error pointing at `[flags]` (one home for
  "flags every target gets").
- Merge: scalar keys fill-if-absent (target wins); list/visibility keys
  **prepend** (default entries stay first, target entries append).
- **Eligibility, not opt-out**: a default never fills a key onto a target
  where it would be illegal — `install`/`public-headers` skip dev/test
  targets; `public-headers` fills only onto installing libraries;
  `runtime-data` fills everywhere (build-time staging is what test runners
  want; multi-target staging is legal via byte-equal dedupe).
- `[target-defaults.cfg.<pred>]` is reserved.

### Patches on dependencies (since wave 1)

`patches = ["path.patch", ...]` — paths relative to the manifest's
directory; bare strings only (the `{ file, strip }` table form is
reserved). Valid on `git`/`url` sources in both dep tables; invalid on
`system = true`. Applied after fetch/extract, before `subdir` is entered and
before configure, in manifest order, via `git apply -p1
--whitespace=nowarn` at the checkout root (url tarballs share the code
path). Exact context, zero fuzz, offset drift tolerated; binary patches
allowed. Any hunk failure is a hard error citing dep, patch file, and hunk,
with a `re-diff against <resolved commit>` hint. Application is atomic (temp
dir + rename); the pristine unpatched source is never mutated.

Identity: patch bytes compose into the **package id**
(`<base>+patches:<hash>`) — patched sources *are* different sources. The
config-hash encoding is untouched and no existing store entry is
invalidated. Renaming a patch file without changing bytes does not re-key.
The lockfile gains ordered `patches = ["blake3:<hex>", ...]` rows; drift
between lock rows and current patch files re-resolves and rebuilds, by
design. `${pin.<dep>.commit}` always reports the base commit.

### System dependencies (since wave 1)

`system = true` — a third source form in the same tables: resolve from the
machine, never build. Mutually exclusive with `git`/`url`, `patches`,
`options`; `needs` *on* a system dep is an error (other deps may `needs` it,
and targets reference its exported targets normally). Optional
`min-version = "1.5"`.

- The **lockfile records the declaration, never the machine**: a system
  dep's lock row is `source = "system"` plus the declared constraints.
  Machine facts (resolved version, paths, file hashes) live only in the
  machine-local sysdep store entry. Probing is *provisioning*, not locking:
  it runs only when the requested target set reaches the sysdep under an
  active predicate — an uninstalled system dep errors at need-time, not
  resolve-time.
- Resolution mode v1 = "cmake": `find_package(<find-package or key>)` with
  the hermetic find restrictions opened for exactly this package. The result
  is a manifest-only store entry (no artifacts), re-resolved when the
  recorded file hashes no longer match disk. A `pkg-config = "<name>"` field
  is **reserved, not implemented**.
- Hashing: a dedicated sysdep hash (resolution mode, resolved version,
  sorted library paths + per-file content hashes, include dirs) enters
  dependents' dep-hashes — store entries downstream of a system dep are
  machine-local *by construction*. Header trees are not hashed (documented
  gap). When an OS update changes the hash, the CLI names the sysdep.
- Not-found errors offer both worlds: declare it fetched (git/url) to build
  hermetically, or install it (`pacman -S zstd` / `brew install zstd`).

**Hermeticity scan.** Every absolute path in a store manifest must be
covered by some hash input — store-rooted, or declared-system. Undeclared
absolute paths (the classic silent Homebrew/`/usr/lib` leak) are an **error
by default**, naming the dep, the path, and both fixes. CLI-only downgrade:
`cpp-pkg build --allow-undeclared-system-libs` (warn; unsupported for
sharing). There is deliberately no manifest knob.

### Builtins (since wave 1)

v1 builtin pseudo-package list: **`Threads::Threads`** only. Referenceable
with no declaration (ladder step 0); claiming it via `exposes-*` is a hard
error whose message is the fix. Expansion is a pure function of toolchain
identity (zero new hash inputs), keyed on the os axis: linux (glibc and musl
alike) → `-pthread` on compile and link of every target whose closure
contains it, emitted before target flags so last-wins overrides hold;
macos/windows → nothing. Extraction rewrites imported `Threads::Threads`
into the symbolic input `builtin:threads`, making store manifests more
platform-portable. (`dl`/`m`/`rt` remain cfg link-flags until evidence.)

### Install & export (since wave 1)

`cpp-pkg install --prefix <dir> [--destdir <dir>] [--config] [--toolchain]
[--list] [targets...]` — builds, then stages FHS layout: `bin/`, `lib/`,
`include/`, `lib/cmake/<CmakeName>/`, `share/<package>/`. `--destdir` (and
`DESTDIR`) stage into `<destdir><prefix>` while baked-in paths refer to
`<prefix>` — the distro-packaging contract. `--list` prints the full staging
plan without writing. Idempotent, overwrite-by-default, never deletes what
it didn't just write; no uninstall.

- `install = true` on a target exports it. Executables → `bin/`. Libraries →
  `lib/` + headers + CMake package files. Exporting a library requires
  `[package].version` (SameMajorVersion); binaries-only installs don't.
- **Headers are derived**: every file under each `includes.public` dir
  (including `${gen}` public dirs) matching `.h .hpp .hh .hxx .inc .ipp`
  installs to `include/<rel-path>`. The declared public interface *is* what
  ships — it cannot desync from include claims, even under cfg projections.
  For exceptional layouts, `public-headers = { base = ".", patterns =
  [...] }` is a **total override** (never merged, not cfg-conditionable;
  `${gen}` is not legal in `base`). Empty derivation for an exported
  library, and same-path-different-bytes collisions, are hard errors;
  byte-equal overlaps dedupe; symlinks are not followed.
- **Emission**: `<CmakeName>Config.cmake` (relocatable IMPORTED targets
  under `namespace::`; public defines/flags/cxx-std as INTERFACE properties;
  private static-lib deps as `$<LINK_ONLY:...>`),
  `<CmakeName>ConfigVersion.cmake` (SameMajorVersion), and
  `cppkg-manifest.json` (the manifest beside the Config, `@prefix@`-
  relative). Invariant: probing the installed Config reproduces
  `cppkg-manifest.json` exactly (modulo prefix).
- **Closure rules**: an unexported local target in an exported closure is a
  hard error (`add install = true or remove the edge`); dev targets/dev-deps
  in an exported closure are errors; `install = true` on a `test` target is
  an error. External deps become `find_dependency(...)` in the Config plus
  `requires` rows (source URL + pin + options + patches) in
  `cppkg-manifest.json`; a patched dep's patch bytes are staged into the
  prefix at `lib/cmake/<CmakeName>/patches/<blake3>.patch`. System deps
  serialize as system requirements, never as resolved paths. Exported
  manifests may not contain absolute paths.
- **`runtime-data`** (fields: `from` required — missing dir is a hard
  error; `patterns` with `!` support, default `**/*`; `to` default = last
  component of `from`): staged at **build time** next to the target's
  output (order-only ninja copy edges — building the target always stages),
  and at install time under `share/<package>/<to>/`. Destination
  collisions: byte-equal sources dedupe (two targets may declare the same
  data); different bytes for one destination is a hard error. Pair with an
  `${install-prefix}` define for baked lookup paths that stay honest in the
  dev tree.
- Out of scope, honestly: `.pc` emission, shared libraries/SONAME, CPack
  packaging (DESTDIR is the packager interface), IMPORTED-executable
  exports.

### `needs` and find_dependency

- Every `needs` entry must be a key of `[dependencies]` (naming a dev-dep
  from a regular dep is an error since wave 1); unknown keys and `needs`
  cycles are errors. `needs` on a `system = true` dep is an error.
- Build order follows `needs` edges. When configuring a dependency, its
  `CMAKE_PREFIX_PATH` contains the store prefixes of the **transitive closure
  of its `needs`** — not just direct entries — because a loaded
  `fmtConfig.cmake` re-runs its own `find_dependency` calls in the same
  configure.
- `needs` edges feed the config hash (via the dep-artifact-hash rule,
  `CPP_PKG_IMPLEMENTATION.md` §3): editing `needs` causes rebuilds, by
  design.
- Both failure shapes are caught and translated: `find_dependency(X)`
  not-found → "add X to [dependencies] and to <dep>.needs"; and
  `find_dependency(X <version>)` version-rejection (a different, more
  confusing CMake error) → an error naming the pinned version vs. the
  requirement.
- `needs` reaching a cfg'd-out dep is a resolve error naming the false
  predicate (since wave 1).

### `find-package` (documented since wave 1; worked before)

When a dependency's `find_package()` name differs from its key
(`googletest` → `GTest`), declare `find-package = "GTest"`; the probe and
emitted find_dependency edges use it. A raw CMake config-not-found error
from the probe is translated into a hint naming this field.

### Profiles and configs

- The selected profile determines `CMAKE_BUILD_TYPE` for every dependency
  (strict same-config propagation; `DESIGN_CHOICES.md`).
- Profile `cxx-flags`/`c-flags`/`link-flags` apply to **consumer targets
  only** — *except* the ABI-affecting class below, which reaches dependency
  builds. Since wave 1, `[flags]` shares exactly this rule; the invariant
  stands: any flag reaching a dep build MUST fold into that dep's config
  hash.
- **ABI-affecting flags propagate to dependency builds** (decided
  2026-08-13): a classification table recognizes ABI-affecting flags/defines
  (`-D_GLIBCXX_DEBUG`, `-D_GLIBCXX_ASSERTIONS`, `-D_GLIBCXX_USE_CXX11_ABI=*`,
  `-D_LIBCPP_HARDENING_MODE=*`, `-stdlib=*`, `-f*-abi*` — extensible), and
  these are injected into every dependency's build (via the generated
  toolchain file) and **folded into each dependency's config hash**, so deps
  rebuild under such profiles — correct by construction rather than
  hard-erroring. Since wave 1 the classifier also unwraps `-Wl,`/`-Wa,`/
  `-Wp,`/`-Xlinker`-style transports before matching. Unrecognized flags
  default to consumer-only. `-fsanitize=*` remains consumer-only with a
  **warning** that dependencies are uninstrumented (ASan interoperates;
  MSan/TSan whole-world instrumentation is out of scope).
- Flags route by language: `cxx-flags` only to the C++ driver, `c-flags`
  only to the C driver.
- Flag ordering is **last-wins by contract** (since wave 1; see Flags —
  Layering): e.g. a profile's `-O2` deliberately overrides the builtin
  config's `-O3`, and a target's private `-O3` overrides both.

### Languages

- Extension table (exhaustive; anything else in `sources` is a **hard
  error**, never silently C++): `.cpp .cc .cxx .c++` → C++; `.c` → C.
  `.C` is a hard error (undecidable on macOS's case-insensitive default
  filesystem; error message suggests renaming). `.m`/`.mm` → clear
  "Objective-C not supported" error.
- `c-std` mirrors `cxx-std` for C sources.
- **Link language rule:** a target containing any C++ source, or any C++
  target/dependency in its link closure, links with the C++ driver.

### Paths and outputs

- Default project build directory: `./build` (per `CPP_PKG.md`); generated
  outputs under `build/gen/` (since wave 1).
  `build/compile_commands.json` is generated (feeds `cpp-pkg build --query`).
- `path`-type dependencies (local trees) are **not** implemented. Recorded
  intent so store assumptions don't foreclose them: path deps will bypass the
  content-addressed store entirely and always rebuild (mutable source has no
  stable hash); nothing else about the store design may assume "all deps live
  in the store".

### Reserved registry (distinct errors today, spellings fixed)

cfg combinators as quoted keys (`"all(linux, gcc)"`) and probe predicates in
the same positions; `apple-clang`; `[target-defaults.cfg.*]`,
`[cfg.*.targets.*]`, `[cfg.*.generate.*]`, `[profiles.*.cfg.*]`; cfg scalar
overrides; `test = true, dev = false`; `expect-signal`; `${pin.self.*}`;
patch `{ file, strip }` form; `pkg-config = "..."` on system deps; flag keys
in `[target-defaults]`; knob sugar (`exceptions`/`rtti`); `frameworks`
field; `cxx-extensions`; `base-config` profiles.

## `CppPkg.lock`

```toml
schema-version = 1

[[package]]
name = "fmt"
source = "git+https://github.com/fmtlib/fmt"
requested = "tag:11.2.0"
commit = "<resolved sha>"           # pin + integrity + re-download reference

[[package]]
name = "zlib"
source = "url+https://zlib.net/zlib-1.3.1.tar.gz"
requested = "sha256:9a93b2b7..."
content-hash = "blake3:<hash of the archive bytes as downloaded>"

[[package]]                         # (since wave 1) patched dependency
name = "absl"
source = "git+https://github.com/abseil/abseil-cpp"
requested = "rev:255c84da..."
commit = "255c84da..."              # always the BASE commit
patches = ["blake3:<hex>"]          # application order; present iff declared

[[package]]                         # (since wave 1) system dependency:
name = "zstd"                       # the DECLARATION, never the machine —
source = "system"                   # resolved versions/paths/hashes live in
requested = "system"                # the machine-local sysdep store entry
min-version = "1.5"                 # present iff declared
```

Grammar is lockfile ABI, pinned here (not left to what the implementation
happens to print): `source` = `git+<url>` | `url+<url>` | `system`
(since wave 1); `requested` = `tag:<tag>` | `rev:<sha>` | `sha256:<hex>` |
`system`; `commit` present iff git; `content-hash` present iff url;
`patches` (since wave 1) present iff declared, blake3 of each patch file's
bytes in application order; `min-version` (since wave 1) present iff
declared on a system dep.

**Locking is eager, provisioning is lazy** (since wave 1): every declared
dependency — dev, cfg-inactive, and system alike — is resolved and locked on
every resolve. The lockfile is the complete declared universe, platform- and
dev-independent, committable from any machine. Fetching, building, and
probing happen only for dependencies the requested build actually reaches.

**Integrity model (decided 2026-08-13):**

- `git` sources: the **commit sha is the content hash** — git commits are
  already content-addressed (tree + history), verification is
  `git rev-parse HEAD` after checkout, and the same sha serves re-download on
  a fresh machine (clone/fetch that commit from `source`). No custom tree
  serialization to specify or maintain; git's hardened SHA-1 is an acceptable
  threat model for now. (A CppPkg-defined canonical tree hash remains the
  recorded fallback if store-level verification independent of git is ever
  needed — e.g. tarball exports of git sources.)
- `url` sources: blake3 of the archive bytes exactly as downloaded (plus the
  user-declared `sha256` checked at fetch time).
- Patched sources (since wave 1): the base pin verifies as above; patch
  bytes are hashed independently into the `patches` rows and into the
  composed package id. Lock-row drift against the current patch files
  re-resolves and rebuilds, by design.
- Submodules remain an **error**: gitlinks do pin exact submodule
  commits, but naive clones don't fetch them and `.gitmodules` URLs are
  mutable — building silently without them is a classic package-manager bug;
  refuse instead. (Detection is by actual gitlink entries, not `.gitmodules`
  presence.)

Written/updated on every resolve; committed to the consumer's VCS.
`options`/`needs` are deliberately absent (they live in `CppPkg.toml` and the
config hash; there is no solver whose resolution they'd affect).
