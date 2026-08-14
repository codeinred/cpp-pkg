# `CppPkg.toml` — v0 schema

Status: **v0, oracle-reviewed** (2026-08-13). Concrete choices and the oracle
verdicts are recorded in `DESIGN_CHOICES.md`; this file is the normative
schema description with an annotated example. Key convention: kebab-case for
all TOML keys (`exposes-namespace`, `cxx-std`).

## Annotated example

```toml
schema-version = 1                  # required; format versioning from day one

[package]
name = "myapp"                      # required; charset [a-zA-Z0-9_-]+
version = "0.1.0"                   # optional in v0 (no solver consumes it)

# ------------------------------------------------------------------
# Toolchain presets (optional). Selected via `cpp-pkg build --toolchain
# <name>`; a path argument (`--toolchain /usr/bin/clang++`) also works. With
# neither, CppPkg auto-detects `c++` on PATH.
[toolchains.gcc-homebrew]
cxx = "g++-15"
cc  = "gcc-15"                      # optional; derived from cxx if omitted
ar  = "gcc-ar-15"                   # optional; detected if omitted
# (target/sysroot/stdlib fields are future additive extensions; toolchain
# *identity* always comes from detection output, never from the preset name)

# ------------------------------------------------------------------
# Profiles: named build flavors. v0 ships exactly the four built-ins, named
# after the CMake configs: debug | release | relwithdebinfo | minsizerel
# (selected via `cpp-pkg build --config debug`; default release).
# `base-config` is RESERVED for future custom profiles (e.g. a "debug-asan"
# with base-config = "debug") so profile names are not forever conflated with
# CMake config names; v0 rejects profiles outside the built-in four.
[profiles.debug]
cxx-flags  = ["-fsanitize=address"] # consumer targets only — see Semantics
c-flags    = []                     # routed to the C driver only
link-flags = ["-fsanitize=address"]

# ------------------------------------------------------------------
# Dependencies: the FULL transitive closure, declared by the user (v0).
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

# Source forms (exactly one per dependency):
#   git + tag | git + rev (commit)   — tag resolved to a commit, pinned in
#                                      CppPkg.lock; git submodules are an
#                                      ERROR in v0 (unsupported, not ignored)
#   url + sha256                     — tarball/zip
# Common fields:
#   options = { KEY = "VALUE" }      — CMake cache options for the dep build.
#                                      Hashed as LITERAL strings: ON/TRUE/1
#                                      are distinct hash inputs by design;
#                                      never "normalize" them (it would
#                                      invalidate every store entry).
#   needs   = ["depkey", ...]        — packages whose config files this dep's
#                                      config requires (find_dependency)
#   exposes-namespace = ["fmt"]      — claim ownership of all targets whose
#                                      namespace is `fmt::` (when the dep key
#                                      doesn't match the exported namespace)
#   exposes-targets   = ["fmt::fmt"] — claim explicit targets; the mapping
#                                      form renames: { "fmt::fmt" = "fmt" }

# ------------------------------------------------------------------
# Targets. Table key = target name; charset [a-zA-Z0-9_-]+ (no "::", so local
# names can never collide with dependency-exported names like "fmt::fmt").
[targets.core]
type    = "static-library"
sources = ["src/core/**/*.cpp"]     # globs resolve in sorted (lexicographic
                                    # byte-order) form at generation time;
                                    # build.ninja regenerates every build in
                                    # v0, so globs stay fresh
cxx-std = 20                        # strict -std=c++20; `cxx-extensions =
                                    # true` (gnu++20) is reserved, default
                                    # false, decided now
includes = { public = ["include"], private = ["src"] }
defines  = { public = ["CORE_API="], private = ["CORE_INTERNAL"] }
dependencies = { public = ["fmt::fmt"], private = ["spdlog::spdlog"] }

[targets.myapp]
type    = "executable"
sources = ["src/main.cpp"]
dependencies = ["core"]             # bare list == all-private (sugar; applies
                                    # uniformly to includes/defines/deps)
```

## Semantics

### Target references and the naming ladder

A `dependencies` entry is either a sibling target name (`core`) or a
dependency-exported target name (`fmt::fmt`). Resolution follows the ladder
(per `CPP_PKG.md`):

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
change. v0 implements strings only.

### Visibility and propagation

`public` entries of a library's `includes`/`defines`/`dependencies` propagate
to its consumers; `private` do not — with one crucial exception: **for a
`static-library`, `private` dependencies propagate as link-only edges**
(CMake's `$<LINK_ONLY:...>` behavior). A static library does not link, so its
private deps' *artifacts* must still reach the final link closure; only their
compile requirements stop. (`myapp → core → private spdlog`: myapp links
spdlog but sees none of its headers/defines.) The manifest IR's separate
compile-edge/link-edge fields carry this directly.

An `interface` visibility bucket and `interface-library` target kind are
deferred (purely additive later); header-only *dependencies* still work in v0
via extraction (INTERFACE imported targets).

### `needs` and find_dependency

- Every `needs` entry must be a key of `[dependencies]`; unknown keys and
  `needs` cycles are errors.
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

### Profiles and configs

- The selected profile determines `CMAKE_BUILD_TYPE` for every dependency
  (strict same-config propagation; `DESIGN_CHOICES.md`).
- Profile `cxx-flags`/`c-flags`/`link-flags` apply to **consumer targets
  only** — *except* the ABI-affecting class below, which reaches dependency
  builds. General (non-ABI) flags reaching deps remains future work, gated on
  explicit opt-in (custom profiles with `base-config` or an
  `apply-to-dependencies` flag); the invariant, already honored by the ABI
  class: any flag reaching a dep build MUST fold into that dep's config
  hash.
- **ABI-affecting flags propagate to dependency builds** (decided
  2026-08-13): a classification table recognizes ABI-affecting flags/defines
  (`-D_GLIBCXX_DEBUG`, `-D_GLIBCXX_ASSERTIONS`, `-D_GLIBCXX_USE_CXX11_ABI=*`,
  `-D_LIBCPP_HARDENING_MODE=*`, `-stdlib=*`, `-f*-abi*` — extensible), and
  these are injected into every dependency's build (via the generated
  toolchain file) and **folded into each dependency's config hash**, so deps
  rebuild under such profiles — correct by construction rather than
  hard-erroring. Unrecognized flags default to consumer-only.
  `-fsanitize=*` remains consumer-only with a **warning** that dependencies
  are uninstrumented (sanitizers are designed to interoperate with
  uninstrumented code — ASan stays useful; MSan/TSan-style whole-world
  instrumentation is out of scope).
- Flags route by language: `cxx-flags` only to the C++ driver, `c-flags`
  only to the C driver.

### Languages

- Extension table (exhaustive; anything else in `sources` is a **hard
  error**, never silently C++): `.cpp .cc .cxx .c++` → C++; `.c` → C.
  `.C` is a hard error in v0 (undecidable on macOS's case-insensitive
  default filesystem; error message suggests renaming). `.m`/`.mm` → clear
  "Objective-C not supported in v0" error.
- `c-std` mirrors `cxx-std` for C sources.
- **Link language rule:** a target containing any C++ source, or any C++
  target/dependency in its link closure, links with the C++ driver.

### Paths and outputs

- Default project build directory: `./build` (per `CPP_PKG.md`).
  `build/compile_commands.json` is generated (feeds `cpp-pkg build --query`).
- `path`-type dependencies (local trees) are **not** in v0. Recorded intent
  so store assumptions don't foreclose them: path deps will bypass the
  content-addressed store entirely and always rebuild (mutable source has no
  stable hash); nothing else about the store design may assume "all deps live
  in the store".

## `CppPkg.lock` (v0)

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
```

Grammar is lockfile ABI, pinned here (not left to what the implementation
happens to print): `source` = `git+<url>` | `url+<url>`; `requested` =
`tag:<tag>` | `rev:<sha>` | `sha256:<hex>`; `commit` present iff git;
`content-hash` present iff url.

**Integrity model (decided 2026-08-13):**

- `git` sources: the **commit sha is the content hash** — git commits are
  already content-addressed (tree + history), verification is
  `git rev-parse HEAD` after checkout, and the same sha serves re-download on
  a fresh machine (clone/fetch that commit from `source`). No custom tree
  serialization to specify or maintain; git's hardened SHA-1 is an acceptable
  v0 threat model. (A CppPkg-defined canonical tree hash remains the recorded
  fallback if store-level verification independent of git is ever needed —
  e.g. tarball exports of git sources — but v0 does not define one.)
- `url` sources: blake3 of the archive bytes exactly as downloaded (plus the
  user-declared `sha256` checked at fetch time).
- Submodules remain an **error in v0**: gitlinks do pin exact submodule
  commits, but naive clones don't fetch them and `.gitmodules` URLs are
  mutable — building silently without them is a classic package-manager bug;
  refuse instead.

Written/updated on every resolve; committed to the consumer's VCS.
`options`/`needs` are deliberately absent (they live in `CppPkg.toml` and the
config hash; v0 has no solver whose resolution they'd affect).
