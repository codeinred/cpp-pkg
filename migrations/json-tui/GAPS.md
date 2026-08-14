# GAPS — json-tui migration, wave-2 edition

Wave-1 findings re-examined after the wave-1 extensions landed. Every
workaround the wave targeted is dissolved — this manifest now mirrors
upstream's CMake structure line for line, with zero suppressions and zero
out-of-band scripts. New findings from exercising the wave-1 features are
under "Remaining" (the install-verb bug is the important one).

Validated 2026-08-14, macOS arm64, Apple clang 21, CMake 4.4, fresh store:
build green (no googletest work), `cpp-pkg test` green (4 gtest cases),
parity 6/6 vs upstream CMake, second build+test full store cache hit.

## Dissolved (wave-1 workaround → wave-1 feature)

1. **`configure_file` version header** (was #1, MAJOR; pin.sh pre-generated
   `gen/src/version.hpp` with sed, version stated 3×) →
   **`[generate.version-header]`** tier-a template step with
   `vars = { CMAKE_PROJECT_VERSION = "${package.version}" }`. Version stated
   once; both consumers reference `${gen}/src`; `--version` output is
   byte-identical to upstream's configure_file build. pin.sh's codegen
   block is deleted.

2. **Submodule-guard false positive on args' 0-byte `.gitmodules`**
   (was #2, MAJOR; forced the commit tarball via url+sha256) → guard now
   triggers on actual gitlink entries (tool fix A.6). Upstream's exact
   spelling `git` + `rev = "114200a9…"` resolves, locks, and builds.

3. **`.tar.xz` rejected** (was #3, MINOR; nlohmann_json via the ~180MB
   -unpacked GitHub tag tarball) → fetch whitelist now accepts
   `.tar.xz`/`.tar.bz2` (tool fix A.7). The dependency is upstream's exact
   112KB release asset `json.tar.xz`, provenance restored, sha256 declared.

4. **No per-target flags** (was #4, MAJOR — broke the build: profile-scope
   flags gave `tests` -Werror, tripping gtest's -Wcharacter-conversion) →
   **per-target `cxx-flags` with visibility** + **`cfg.unix`**: the warning
   set is `{ private = [...] }` on `json-tui-lib` and `json-tui` under
   `[targets.*.cfg.unix]` (transcribed: `if (NOT WIN32)`), absent from
   `tests` exactly as upstream shields it; `-fno-exceptions` is
   `{ public = [...] }` on the lib and propagates to both consumers
   (verified on compile lines). Both duplicated `[profiles.*]` stanzas and
   the `-Wno-error=character-conversion` suppression are deleted — zero
   suppressions remain.

5. **Dep headers arrived `-I`, not `-isystem`** (was #4b, MAJOR,
   co-culprit of the break) → imported-target interface includes now
   classify as system includes at manifest ingestion (tool fix A.1); every
   dep include on every compile line is `-isystem` (verified), matching the
   CMake behavior extraction claims to replicate. No manifest surface
   needed.

6. **`find-package` undocumented** (was #10, MINOR) → documented in
   CPPKG_TOML.md; `find-package = "GTest"` on the googletest dev-dep works
   as before.

7. **`Threads::Threads` forced arbitrary ownership** (was #11, MINOR;
   `exposes-targets = ["Threads::Threads"]` on ftxui) → builtin
   pseudo-package (ladder step 0). The ownership line is deleted; both
   ftxui's and googletest's imports resolve to the builtin.

8. **No test story** (was #5, MAJOR) → **`[dev-dependencies]` +
   `test = true` + `cpp-pkg test`**: googletest moved to
   `[dev-dependencies]`; `tests` is `test = true` (leaves the default
   build — upstream's `JSON_TUI_BUILD_TESTS=OFF` default, translated);
   `cpp-pkg build` does zero googletest store work and, with the committed
   lockfile, zero network (verified on a fresh store); `cpp-pkg test`
   provisions it lazily and runs the gtest binary (zero `[[run]]` entries =
   the one default invocation = the whole 4-case suite). Locking stays
   eager: the lockfile carries googletest from any machine.

9. **Old-CMake dep under CMake ≥ 4** (was #6, MINOR) —
   `CMAKE_POLICY_VERSION_MINIMUM = "3.5"` as an ordinary dep option remains
   the working pattern for the 2021 googletest pin; unchanged, still fine.
   (The A.9 error-translation fix was not exercised here since the option
   is declared up front.)

10. **Directory-scoped `add_definitions`** (was #7, MINOR; modeled as a
    PUBLIC define on the lib — reached the right TUs but by propagation,
    not by scope) → **`[target-defaults]`**
    `defines = { private = ["JSON_NOEXCEPTION"] }` hits all three targets
    directly, the exact translation of directory scope. `cxx-std = 17`
    rides the same table as a fill-if-absent scalar (`tests` overrides
    with 20 — upstream's `cxx_std_17`/`cxx_std_20` split).

11. **Install not expressible** (was #8, MINOR) → `install = true` on the
    `json-tui` executable is the declared translation of
    `install(TARGETS json-tui RUNTIME)`. The *declaration* is now schema
    syntax; the *verb* is broken for this shape — see Remaining #1.

(Wave-1 #9, the positive finding on component deps, stands: the
FetchContent→declared-deps core needed no name-resolution workarounds, and
is now one line shorter with the Threads builtin.)

## Remaining

### 1. NEW BUG — `cpp-pkg install` of an executable drags its non-exported static lib into staging (install-export) — MAJOR

`install = true` on `json-tui` alone (upstream ships only the RUNTIME
binary), and the verb hard-errors:

    error: target 'json-tui-lib' is exported but its header derivation is
    empty (public include dirs: []) — add public headers, a public-headers
    override, or remove install = true

json-tui-lib has `install = false`. The two halves of shim.rs disagree:

- `validate_exported_closure` (shim.rs:560) is correct per spec §6.3 and
  its own doc comment — "Executables statically link their closure and
  impose no such rule" — so validation passes.
- `plan_install` (shim.rs:~866) then closes the *selection* over local dep
  edges unconditionally, with a comment claiming "closure members are
  exported per validation" — false for executables, since validation
  deliberately exempted them. Every selected static-library member is then
  staged to `lib/` and run through `derive_headers` regardless of its own
  `install` flag → hard error for any internal header-less lib.

Control experiment confirming the mechanism: giving the lib public
includes makes the plan succeed but stage `lib/libjson-tui-lib.a`, five
headers, and `lib/cmake/json-tui/*` — none of which upstream installs, for
a target whose manifest still says `install = false`.

Consequence: **`install = true` on a product executable that links any
local static library is un-installable** — precisely the "one line each"
shape §6.8's migration note promises for ninja and json-tui (ninja's
`libninja` will hit this identically). Fix direction: the selection
closure should expand only through *exported libraries'* local edges (their
archives must reach consumers); an executable's local static libs are
linked in and need no staging. Both `install` (all-exported) and
`install json-tui` (named) forms fail today. The manifest keeps the honest
`install = true`; blocked-with-diagnosis over fake green.

### 2. Per-case test discovery (testing-story) — MINOR, acknowledged deferred

Upstream's `gtest_discover_tests` registers each of the 4 cases as its own
CTest entry; `cpp-pkg test` runs the gtest binary as one invocation (pass =
exit 0). Fine at this scale — a failing case still fails the invocation and
the captured output is replayed — but there is no per-case reporting or
filtering below target granularity (`-- --gtest_filter=...` passthrough is
the manual escape hatch). Matches the wave's own deferred registry ("case
discovery / output-matching test harnesses").

### 3. `compile_commands.json` is regenerated per-verb, for that verb's target set only (schema-ergonomics) — MINOR

After `cpp-pkg build` it contains the 6 default-set TUs (no
`expander_test.cpp`); after `cpp-pkg test` it contains `expander_test.cpp`
but has *lost* `main.cpp`. Whichever verb ran last wins, so clangd/IDE
users get broken navigation for the other half of the project. A union
(regenerate entries for all known targets, or merge instead of overwrite)
would fix it.

### 4. CPack packaging (install-export) — deliberate non-goal, unchanged

Upstream's DEB/RPM/DMG generator matrix has no cpp-pkg surface; `--destdir`
is the packager interface, per the wave's out-of-scope ruling. Recorded,
not contested. Likewise `JSON_TUI_CLANG_TIDY` (a lint-driver option, not a
build product) is out of scope.
