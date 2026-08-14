# Migration: json-tui v1.4.2

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
- **nlohmann_json v3.12.0** (release tarball `json.tar.xz`) — PUBLIC on the
  lib (its header appears in `main_ui.hpp`)
- **args** (Taywee/args, pinned commit `114200a9…`) — `taywee::args`,
  PRIVATE on the executable
- **googletest** (pinned commit `23ef2955…`) — test-only, via
  `cmake/test.cmake`, only when `-DJSON_TUI_BUILD_TESTS=ON`

Other build features exercised: `configure_file` codegen of `version.hpp`
from `version.hpp.in`; per-target flags (`-Wall -Wextra -pedantic -Werror
-Wmissing-declarations -Wshadow` PRIVATE, `-fno-exceptions` PUBLIC,
directory-scoped `-DJSON_NOEXCEPTION`); `install(TARGETS json-tui RUNTIME)`;
CPack packaging; `gtest_discover_tests` CTest registration.

## Migration approach

All four FetchContent deps become declared `[dependencies]` (ftxui and args
as git pins, nlohmann_json as a url tarball, googletest as a git rev). All
of them install CMake config files, so tier-2 probe extraction covers them;
the exported names used are exactly what the store manifests report:
`ftxui::screen|dom|component`, `nlohmann_json::nlohmann_json`,
`taywee::args`, `GTest::gtest_main`.

Workarounds (each is gap data — see `GAPS.md`):

1. **version.hpp codegen** — `pin.sh` pre-generates `gen/src/version.hpp`
   with `sed` (the moral equivalent of `configure_file`); `gen/src` is a
   private include dir of the two targets that need it.
2. **`.tar.xz` unsupported** — nlohmann_json uses the GitHub tag `.tar.gz`
   tarball instead of upstream's `json.tar.xz` release asset (same v3.12.0
   sources).
3. **args as tarball** — upstream's exact git pin is refused by the v0
   submodule guard, which false-positives on args' *empty* `.gitmodules`
   (no gitlinks exist at that commit); the commit `.tar.gz` works.
4. **Per-target flags** — upstream's warning/`-fno-exceptions` set moved to
   profile `cxx-flags` (applies to all consumer targets, including `tests`,
   which upstream deliberately exempts from `-Werror`). This genuinely broke:
   gtest's `gtest-printers.h` trips `-Wcharacter-conversion` under C++20 —
   also because cpp-pkg emits dep includes as `-I` where CMake uses
   `-isystem` for imported targets — worked around with
   `-Wno-error=character-conversion`.
5. **`find-package = "GTest"`** — the probe's `find_package(<depkey>)`
   default fails for googletest (config is `GTestConfig.cmake`); the
   override field exists in the implementation but is undocumented.
6. **`Threads::Threads` ownership** — both ftxui's and googletest's configs
   import it; the ambiguity error's suggested one-liner
   (`exposes-targets` on ftxui) resolves it.
7. **Tests always on** — no option system / test kind; `tests` is a plain
   executable target, run manually instead of via CTest.
8. **googletest under CMake ≥ 4** — the pinned commit needs
   `CMAKE_POLICY_VERSION_MINIMUM=3.5`, passed as an ordinary dep `option`
   (works; upstream's own CMake build needs the same flag on the command
   line).

No `patches/` directory: zero upstream source-tree edits were needed — the
only synthesized file (`gen/src/version.hpp`) is generated out-of-tree by
`pin.sh`.

Not migrated (out of v0 scope, noted as gaps): `install(TARGETS)` /
CPack packaging, the `JSON_TUI_CLANG_TIDY` option, CTest registration.

Parity result (2026-08-14, macOS arm64, Apple clang 21): 6/6 PASS —
`--version`, `--help`, `--keybinding`, bad-JSON stderr+exit-code, 500-byte
ANSI UI render (byte-identical), 4/4 gtest cases. Second `cpp-pkg build`
after wiping `./build`: zero "building dependency" lines — all four deps
served from the store.

## Reproduce

```sh
cd $(mktemp -d)
sh /opt/claude/cpp-pkg/migrations/json-tui/pin.sh
cd upstream
CPPKG_STORE=/path/to/store cpp-pkg build     # builds deps + all 3 targets
./build/json-tui --version                   # -> 1.4.2
./build/tests                                # 4 gtest cases
```

## Parity protocol (vs. upstream CMake build)

Upstream reference build (CMake 4.4 needs the policy floor for googletest):

```sh
cmake -S upstream -B cmake-build -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DJSON_TUI_BUILD_TESTS=ON -DCMAKE_POLICY_VERSION_MINIMUM=3.5
ninja -C cmake-build
```

Then: `sh parity.sh <cmake-build-dir> <cppkg-build-dir>` (the cppkg build
dir is `upstream/build`). Compared observables (all diffed byte-for-byte,
non-tty):

1. `json-tui --version` (exercises the codegen parity)
2. `json-tui --help`
3. `json-tui --keybinding` (FTXUI table render to redirected stdout)
4. `printf '{"bad":' | json-tui` — stderr parse error + exit code
5. `cat sample.json | timeout 5 json-tui` with stdout redirected — the
   rendered ANSI frames of the interactive UI (deterministic at the default
   non-tty screen size)
6. `tests` — gtest run, 4 tests pass in both
