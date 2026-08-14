# `CppPkg.toml` — v0 schema

Status: **draft** (2026-08-13). Concrete choices recorded in
`DESIGN_CHOICES.md`; this file is the normative schema description with an
annotated example.

## Annotated example

```toml
schema-version = 1                  # required; format versioning from day one

[package]
name = "myapp"                      # required; [a-zA-Z0-9_-]+
version = "0.1.0"                   # optional in v0 (no solver consumes it)

# ------------------------------------------------------------------
# Toolchain presets (optional). Selected via `cppkg build --toolchain <name>`;
# a path argument (`--toolchain /usr/bin/clang++`) also works. With neither,
# CppPkg auto-detects `c++` on PATH.
[toolchains.gcc-homebrew]
cxx = "g++-15"
cc  = "gcc-15"                      # optional; derived from cxx if omitted
ar  = "gcc-ar-15"                   # optional; detected if omitted

# ------------------------------------------------------------------
# Per-config flag additions (optional). Configs are the CMake-compatible set:
# debug | release | relwithdebinfo | minsizerel. Selected via
# `cppkg build --config debug` (default: release).
[profiles.debug]
cxx-flags  = ["-fsanitize=address"]
link-flags = ["-fsanitize=address"]

# ------------------------------------------------------------------
# Dependencies: the FULL transitive closure, declared by the user (v0).
# Keys are CppPkg-local package names (used in the lockfile and store);
# consumers reference the *targets* a package exports, not the package name.
[dependencies]
fmt    = { git = "https://github.com/fmtlib/fmt", tag = "11.2.0" }
spdlog = { git = "https://github.com/gabime/spdlog", tag = "v1.15.3",
           options = { SPDLOG_FMT_EXTERNAL = "ON" },
           needs = ["fmt"] }        # explicit find_dependency edge (see below)
zlib   = { url = "https://zlib.net/zlib-1.3.1.tar.gz",
           sha256 = "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23" }

# Source forms (exactly one per dependency):
#   git + tag | git + rev (commit)     — tag is resolved to a commit and
#                                        pinned in CppPkg.lock
#   url + sha256                       — tarball/zip
# Common fields:
#   options = { KEY = "VALUE" }        — CMake cache options for the dep build
#   needs   = ["name", ...]            — packages this dep's config requires
#                                        (find_dependency); drives build order
#                                        and CMAKE_PREFIX_PATH assembly

# ------------------------------------------------------------------
# Targets. Table key = target name (no "::" allowed, so local names can never
# collide with dependency-exported names like "fmt::fmt").
[targets.core]
type    = "static-library"
sources = ["src/core/**/*.cpp"]     # globs allowed; resolved at generation
                                    # time (build.ninja is regenerated every
                                    # build in v0, so globs stay fresh)
cxx-std = 20                        # lowered per-toolchain (e.g. -std=c++20)
includes = { public = ["include"], private = ["src"] }
defines  = { public = ["CORE_API="], private = ["CORE_INTERNAL"] }
dependencies = { public = ["fmt::fmt"], private = ["spdlog::spdlog"] }

[targets.myapp]
type    = "executable"
sources = ["src/main.cpp"]
dependencies = ["core"]             # bare list == all-private (sugar)
```

## Semantics

- **Target reference namespace.** A `dependencies` entry is either a sibling
  target name (`core`) or a dependency-exported target name (`fmt::fmt`, as
  extracted into the manifest). The namespaces cannot collide because local
  target names may not contain `::`. Un-namespaced tier-3 exports will be
  namespaced by the extractor later (`<pkg>::<target>`), preserving this rule.
- **public/private propagation** mirrors CMake's PUBLIC/PRIVATE and exists in
  v0 (C++ cannot do without it): `public` entries of a library's `includes`/
  `defines`/`dependencies` propagate to its consumers; `private` do not.
  `$<LINK_ONLY>`-style link-only edges arise from *extracted* manifests, not
  from `CppPkg.toml` syntax, in v0. An `interface` visibility bucket
  (header-only libraries) is deliberately deferred with `interface-library`
  targets.
- **`needs` on dependencies** is required in v0 whenever a dep's config file
  calls `find_dependency`: CppPkg builds `needs` first and places them on the
  dep's `CMAKE_PREFIX_PATH`. A `find_dependency` that fails anyway produces a
  clear error naming the missing package and suggesting a `needs`/
  `[dependencies]` addition. (Auto-discovery of the edge from the failed
  configure is future work.)
- **Configs propagate strictly**: the selected profile's config is the
  `CMAKE_BUILD_TYPE` for every dependency; profile `cxx-flags` apply to
  consumer targets only, never to dependency builds (deps are configured only
  by `options` + the toolchain — keeps the store hash meaningful).
- **Target kinds in v0**: `executable`, `static-library`. (`shared-library`,
  `interface-library` on the TODO.)
- **C sources** are allowed in `sources`; language selected per-file by
  extension (`.c` → C driver flags, `c-std` field analogous to `cxx-std`).

## `CppPkg.lock` (v0)

```toml
schema-version = 1

[[package]]
name = "fmt"
source = "git+https://github.com/fmtlib/fmt"
requested = "tag:11.2.0"
commit = "<resolved sha>"
content-hash = "blake3:<hash of raw download store entry>"
```

Written/updated on every resolve; verification behavior may lag the format
(recorded decision). Committed to the consumer's VCS.
