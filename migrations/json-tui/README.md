# Migration: json-tui v1.4.2 (wave-2 edition)

Upstream: https://github.com/ArthurSonzogni/json-tui
Ref: tag `v1.4.2` = commit `717b1f9f6fe261faf4c4ee999a2d28d04b152595`

## What the project is

A terminal UI for viewing JSON documents, by the FTXUI author. ~10 C++17
source files: a static library (`json-tui-lib`) with the UI components, an
executable (`json-tui`), and a gtest executable (`tests`). Upstream is a
CMake 3.24 build that pulls every dependency via FetchContent (with
`FIND_PACKAGE_ARGS` fallback):

- **ftxui v7.0.0** (git tag) — three consumed components:
  `ftxui::screen`, `ftxui::dom`, `ftxui::component` (PRIVATE on the lib)
- **nlohmann_json v3.12.0** (release asset `json.tar.xz`) — PUBLIC on the
  lib (its header appears in `main_ui.hpp`)
- **args** (Taywee/args, pinned commit `114200a9…`) — `taywee::args`,
  PRIVATE on the executable
- **googletest** (pinned commit `23ef2955…`) — test-only, via
  `cmake/test.cmake`, only when `-DJSON_TUI_BUILD_TESTS=ON`

Other build features exercised: `configure_file` codegen of `version.hpp`;
per-target flags (warnings PRIVATE under `if (NOT WIN32)`, `-fno-exceptions`
PUBLIC, directory-scoped `-DJSON_NOEXCEPTION`); `install(TARGETS json-tui
RUNTIME)`; CPack packaging; `gtest_discover_tests` CTest registration.

## Migration approach (wave 2 — all wave-1 workarounds dissolved)

Every wave-1 workaround this project carried is now real schema syntax; the
manifest mirrors upstream's structure line for line:

- `configure_file` → **`[generate.version-header]`** template step (`@VAR@`
  substitution of `${package.version}`); `pin.sh`'s sed block is deleted and
  the version is stated exactly once.
- Per-target flags → **target `cxx-flags`** with visibility: the warning set
  is private on `json-tui-lib`/`json-tui` under **`cfg.unix`** (transcribed
  from upstream's `if (NOT WIN32)` guard — this is also the Linux branch);
  `-fno-exceptions` is public on the lib. The duplicated
  `[profiles.release]`/`[profiles.debug]` stanzas are deleted, and the
  wave-1 `-Wno-error=character-conversion` suppression is gone with **zero**
  replacements (gtest headers now arrive `-isystem`, per CMake's
  imported-target behavior).
- `add_definitions(-DJSON_NOEXCEPTION)` → **`[target-defaults]`** private
  define (directory scope translated exactly); `cxx-std = 17` is a default
  scalar, overridden to 20 by `tests` — upstream's
  `target_compile_features` split.
- googletest → **`[dev-dependencies]`**; `tests` is **`test = true`** and
  leaves the default build. `cpp-pkg build` does zero store work for
  googletest (verified); `cpp-pkg test` provisions it, builds `tests`, and
  runs the gtest binary (the default zero-`[[run]]`-entries invocation = the
  whole 4-case suite).
- args → upstream's exact **git+rev pin** (the wave-1 submodule-guard
  false positive on the 0-byte `.gitmodules` is fixed; the commit-tarball
  workaround is deleted).
- nlohmann_json → upstream's exact **`.tar.xz` release asset** (the
  whitelist now accepts it; the ~180MB-unpacked GitHub tag-tarball
  substitute is deleted, provenance restored).
- `Threads::Threads` → **builtin**; the arbitrary
  `exposes-targets = ["Threads::Threads"]` ownership line on ftxui is
  deleted.
- `install(TARGETS json-tui RUNTIME)` → **`install = true`** on the
  executable. NOTE: `cpp-pkg install` currently fails on this manifest —
  a bug in the install verb's selection closure drags the non-exported
  internal static lib into staging (see GAPS.md "Remaining" #1). The
  declaration is correct per the spec; the verb is not.

Not migrated (still out of scope, deliberate): CPack packaging (DESTDIR is
the packager interface), `JSON_TUI_CLANG_TIDY`, per-case CTest discovery
(`gtest_discover_tests`) — `cpp-pkg test` treats the gtest binary as one
invocation.

## Results (2026-08-14, macOS arm64, Apple clang 21, fresh store)

- `cpp-pkg build`: 3 deps (ftxui, nlohmann_json, args) + lib + exe; no
  googletest work; `build/gen/src/version.hpp` generated as a build edge.
- `cpp-pkg test`: googletest provisioned lazily, `tests` built and run —
  `1 passed, 0 failed` (4 gtest cases inside the invocation).
- Parity 6/6 PASS vs upstream CMake build (`--version`, `--help`,
  `--keybinding`, bad-JSON stderr+exit, 500-byte ANSI UI render
  byte-identical, gtest output).
- Second `cpp-pkg build` + `cpp-pkg test` after wiping `./build`: zero
  "building dependency" lines — all four deps served from the store.
- Compile-line audit: lib/exe TUs get the six warnings + `-Werror` (cfg.unix
  matched) at C++17; `tests` TU has **no** `-Werror`, inherits public
  `-fno-exceptions`, compiles at C++20; all dep includes are `-isystem`;
  `JSON_NOEXCEPTION` on every TU.
- `cpp-pkg install --prefix …`: FAILS (tool bug, GAPS.md Remaining #1).

## Reproduce

```sh
cd $(mktemp -d)
sh /opt/claude/cpp-pkg/migrations/json-tui/pin.sh
cd upstream
CPPKG_STORE=/path/to/store cpp-pkg build   # deps + lib + exe (no googletest)
./build/json-tui --version                 # -> 1.4.2 (generated header)
CPPKG_STORE=/path/to/store cpp-pkg test    # provisions gtest, runs the suite
```

## Parity protocol (vs. upstream CMake build)

Upstream reference build (CMake 4.4 needs the policy floor for googletest):

```sh
cmake -S upstream -B cmake-build -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DJSON_TUI_BUILD_TESTS=ON -DCMAKE_POLICY_VERSION_MINIMUM=3.5
ninja -C cmake-build
```

Then: `sh parity.sh <cmake-build-dir> <cppkg-build-dir>` (the cppkg build
dir is `upstream/build`; run `cpp-pkg test` first so `build/tests` exists).
Compared observables (all diffed byte-for-byte, non-tty):

1. `json-tui --version` (exercises `[generate]` parity with configure_file)
2. `json-tui --help`
3. `json-tui --keybinding` (FTXUI table render to redirected stdout)
4. `printf '{"bad":' | json-tui` — stderr parse error + exit code
5. `cat sample.json | timeout 5 json-tui` with stdout redirected — the
   rendered ANSI frames of the interactive UI (deterministic at the default
   non-tty screen size)
6. `tests` — gtest run, 4 tests pass in both
