# GAPS — json-tui migration

Every friction point hit while porting json-tui v1.4.2 from
CMake+FetchContent to cpp-pkg, keyed to the design questions.

## 1. `configure_file` version header (codegen-escape-hatch) — MAJOR

Upstream generates `${BINARY_DIR}/src/version.hpp` from `src/version.hpp.in`
by substituting `@CMAKE_PROJECT_VERSION@`. This is the single most common
codegen pattern in the wild (version/config headers), and cpp-pkg has no way
to express it. Workaround: `pin.sh` pre-generates `gen/src/version.hpp` with
`sed`, and the version string is now duplicated in three places (pin.sh, the
manifest's `[package].version`, upstream's CMakeLists).

What an escape hatch would have needed here, in ascending generality:
(a) built-in `configure-file`-style substitution with access to
`[package].version` (covers this project outright and probably the majority
of real-world cases); (b) a generic "run command, declare outputs, outputs
land on an include path" step with correct rebuild tracking. Notably (a)
requires no arbitrary-command machinery at all.

## 2. Submodule guard false-positives on empty `.gitmodules` (dep-provisioning) — MAJOR

Declaring upstream's exact pin `Taywee/args @ 114200a9` as `git`+`rev` is
refused: "repository ... uses git submodules (gitlink entries or .gitmodules
present)". But at that commit `.gitmodules` is **0 bytes** and `git ls-tree
-r HEAD` shows **no** 160000 gitlink entries — there are no submodules,
only a stale empty file. The refuse-don't-silently-skip policy is right; the
detector should trigger on actual gitlink entries (or a .gitmodules that
parses to ≥1 submodule), not on file presence. Workaround: the GitHub commit
tarball via `url`+`sha256` (error message helpfully suggests this).

## 3. Only `.tar.gz`/`.tgz`/`.zip` url deps (dep-provisioning) — MINOR

Upstream fetches nlohmann's release asset `json.tar.xz`. cpp-pkg rejects
`.tar.xz` (fetch.rs whitelist). Workaround: GitHub tag tarball (`.tar.gz`)
of the same release — but that is a *different artifact* (full repo, ~180MB
unpacked with tests/docs, vs the trimmed 43MB release asset), so the
workaround costs download size and loses upstream's exact provenance.
`tar -xJf`/`-xf` is the same system tar already shelled out to; supporting
`.tar.xz`/`.tar.bz2` is nearly free.

## 4. No per-target compile flags (per-target-flags) — MAJOR, broke the build

Upstream's flag structure is genuinely per-target and per-visibility:

- `-Wall -Wextra -pedantic -Werror -Wmissing-declarations -Wshadow`
  PRIVATE on `json-tui-lib` and `json-tui` only;
- `-fno-exceptions` PUBLIC on `json-tui-lib` (propagates to consumers);
- the gtest `tests` target is deliberately exempt from the warning set
  (it still inherits `-fno-exceptions` through the lib's PUBLIC flag).

cpp-pkg's only flag surface is profile-level `cxx-flags`, which hit every
consumer target equally and must be duplicated per profile (`release` and
`debug` blocks are copy-paste). This actually failed, not hypothetically:
`tests` got `-Werror`, and gtest's `gtest-printers.h:483` trips
`-Wcharacter-conversion` under C++20 with Apple clang 21 → hard error in a
target the upstream author explicitly shields from `-Werror`. Workaround:
`-Wno-error=character-conversion` in the profile flags (global demotion of
one diagnostic — a scope reduction upstream does not need). What's missing:
`cxx-flags`/`link-flags` on `[targets.*]` with public/private visibility
(the existing includes/defines visibility model extends naturally;
`-fno-exceptions` PUBLIC is precisely a propagating compile requirement).

Related ergonomic wart (schema-ergonomics, minor): identical profile flag
blocks must be repeated for each of the four built-in profiles; flags that
should apply to *all* profiles have no home.

## 4b. Dependency includes are `-I`, not `-isystem` (per-target-flags) — MAJOR

Co-culprit of the failure above, and independently wrong: CMake treats
imported targets' `INTERFACE_INCLUDE_DIRECTORIES` as SYSTEM includes by
default (`NO_SYSTEM_FROM_IMPORTED` exists to opt *out*), so upstream
consumers never see dep-header warnings. cpp-pkg's manifest has a
`system_includes` bucket and the toolchain layer can emit `-isystem`
(toolchain.rs:264), but the tier-2 probe only fills the bucket from explicit
SYSTEM properties, so store deps' headers arrive via `-I` and are exposed to
the consumer's warning flags. gtest, and any dep with warnings under a
strict flag set, breaks projects that build `-Werror`. Fix direction: the
probe (or manifest ingestion) should classify imported-target interface
includes as system by default, matching the CMake behavior the extraction
claims to replicate.

## 10. Probe `find_package` name defaults to the dep key; override is undocumented (schema-ergonomics) — MINOR

`find_package(googletest)` fails — the installed config is
`GTestConfig.cmake`. The implementation already has the right escape hatch
(`find-package = "GTest"` on the dependency, schema.rs:133), and it worked
first try — but it is absent from CPPKG_TOML.md, so a user only discovers
it by reading the source. Document it; also consider having the probe retry
with common case variants or scan `lib/cmake/*/ *Config.cmake` in the just-
installed prefix (the probe already knows the exact install dir, so the
correct name is discoverable automatically).

## 11. Shared system-module targets (`Threads::Threads`) force arbitrary ownership (dep-provisioning) — MINOR

Both ftxui's and googletest's configs `find_package(Threads)`; both
manifests export `Threads::Threads`; any reference to it is ambiguous until
one dependency claims it via `exposes-targets`. The error message is
excellent (it names both candidates and spells out the exact fix), and the
one-line workaround (`exposes-targets = ["Threads::Threads"]` on ftxui)
resolved everything. But the semantics is a small lie — ftxui does not own
Threads; it is a CMake find-module target that *any* config may import.
A curated builtin list of well-known system module targets (Threads::,
Threads/OpenMP/X11/OpenGL::, etc.) treated as shared-and-identical would
remove the arbitrary-ownership declaration from nearly every multi-dep
migration on day one.

## 5. No test story (testing-story) — MAJOR

Upstream: tests are opt-in (`JSON_TUI_BUILD_TESTS=ON`), googletest is
fetched only then, `gtest_discover_tests` registers per-case CTest entries.
In the port:

- `tests` is an always-built plain executable; `cpp-pkg build` builds
  googletest even for someone who only wants the app. No `[dev-dependencies]`
  / per-target dep gating.
- No `cpp-pkg test`: running is manual (`./build/tests`). Nothing discovers
  gtest cases or reports them individually.
- No way to express "this target only exists when testing" —
  conditional-sources and testing-story meet here; a cargo-style implicit
  test profile (test targets + test-only deps built only on `cpp-pkg test`)
  would cover this project completely.

## 6. Old-CMake deps under CMake ≥ 4 (dep-provisioning) — MINOR (works, worth blessing)

The pinned googletest commit (2021) declares `cmake_minimum_required` < 3.5
and CMake 4.4 refuses to configure it. Passing
`CMAKE_POLICY_VERSION_MINIMUM = "3.5"` as an ordinary dep `option` works —
good — but this will hit *every* migration that pins an old dep, and users
have to know the incantation. Worth either documenting as the blessed
pattern or translating CMake's error into a hint the way find_dependency
failures already are. (Upstream's own CMake build needs the same flag.)

## 7. Directory-scoped `add_definitions` (schema-ergonomics) — MINOR

Upstream's `add_definitions(-DJSON_NOEXCEPTION)` applies to all targets in
the directory. The port models it as a PUBLIC define on `json-tui-lib`,
which reaches all three targets only because they all happen to depend on
the lib. That's a faithful-enough translation here, but the mapping required
understanding CMake scoping subtleties; a top-level `[defines]`/"applies to
all project targets" block would make such ports mechanical.

## 8. Install/packaging not expressible (install-export) — MINOR here

Upstream has `install(TARGETS json-tui RUNTIME DESTINATION bin)` plus a full
CPack section (DEB/RPM/DMG/...). cpp-pkg has no `install` verb, so the port
simply drops this. For a leaf application it only costs `make install`
convenience (minor), but it is the same missing surface that would block
migrating any *library* project: nothing in the schema says what a package
exports to the outside world.

## 9. Component deps were near-friction-free (positive finding)

FTXUI's three components resolved exactly as declared
(`ftxui::screen|dom|component`, PRIVATE on a static lib → link-only
propagation to the exe; the store manifest also exposes the aggregate
`ftxui::ftxui`), `taywee::args` and `GTest::gtest_main` resolved via the
unique-name ladder without `exposes-*` declarations, and the PUBLIC/PRIVATE
dependency split mapped 1:1 from `target_link_libraries`. Apart from the
`Threads::Threads` ownership line (#11), the FetchContent→declared-
dependency conversion itself — the core bet of the tool — needed no name-
resolution workarounds, and the second build was a full store cache hit for
all four dependencies.
