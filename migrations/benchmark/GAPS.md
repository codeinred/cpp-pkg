# GAPS — google/benchmark v1.9.5 migration, wave-2 edition

Wave 1 recorded 8 gaps. The wave-1 extensions dissolve the substance of 7 of
them; this file records each workaround → feature mapping (verified by
rebuilding, re-running parity, and running the full suite on macOS/arm64/
AppleClang 21), then what honestly remains.

## Dissolved (workaround → feature)

1. **Hardcoded `BENCHMARK_VERSION="v1.9.5"`** (gap 1) →
   `defines = { private = ['BENCHMARK_VERSION="v${package.version}"'] }`.
   Stated once, in `[package].version`; drift-on-repin is dead. Verified the
   strong way: `benchmark.cc.o` (the TU that embeds the string) is
   bit-identical to the reference object produced from `git describe`. The
   git-describe/store-strips-`.git` trap is gone with **no codegen step at
   all** — the interpolated define was the whole need. (`${pin.self.*}` for
   dependency-mode version stamping stays reserved; see Remaining.)

2. **Transcribed macOS-only probe defines** (gap 2) → `cfg` sub-tables with
   `# transcribed:` comments naming each upstream check:
   - `HAVE_STD_REGEX` / `HAVE_STEADY_CLOCK`: true on every vocabulary
     platform → unconditional `[target-defaults]` private defines (global
     upstream via `add_compile_definitions`; test TUs include `src/re.h` and
     must agree on the regex backend).
   - `HAVE_THREAD_SAFETY_ATTRIBUTES`: compiler-conditional, not
     OS-conditional → `cfg.clang` blocks (defining it under gcc would
     -Werror on the clang-only attributes).
   - `BENCHMARK_HAS_PTHREAD_AFFINITY` + `-lrt`:
     `[targets.benchmark.cfg.linux]` defines/link-flags — the Linux branch
     written from upstream's build logic (probe TRUE on glibc,
     `check_library_exists(rt shm_open)` TRUE), awaiting S5 validation.
   - `shlwapi.lib`: `[targets.benchmark.cfg.windows]` link-flags,
     accepted-but-false here, validated-not-expanded as spec'd.

3. **No per-target flags** (gap 3) → three surfaces, all used:
   - warning battery + visibility preset: duplicated
     `[profiles.release]`/`[profiles.debug]` stanzas deleted → one `[flags]`
     block + `[flags.cfg.clang]` for `-Wshorten-64-to-32`/`-Wthread-safety`
     (upstream probes flags per compiler; with `-Werror` in the battery an
     unconditional block would hard-break gcc).
   - test-dir `-Wno-unused-variable`: private `cxx-flags` on the test
     targets it belongs to. **Wave 1's global `-Wno-unused-but-set-variable`
     deviation is deleted outright** — with upstream's own suppression in
     its proper scope, nothing else was needed; the whole suite compiles
     under the full `-Werror` battery with zero added suppressions.
   - `donotoptimize_test` `COMPILE_FLAGS "-O3"` +
     `-Werror=deprecated-declarations`: private target flags, overriding the
     profile's level by documented last-wins layering.
   - `-Wsuggest-override` correctly dropped (upstream adds it only with
     testing OFF; this manifest carries the suite).

4. **Hand-listed 19 sources / no conditionals** (gap 4) →
   `sources = ["src/*.cc", "!src/benchmark_main.cc"]` (upstream's
   glob-minus-one restored; new upstream files are now picked up instead of
   becoming silent link errors) + the `cfg` branches above. Solaris `kstat`
   stays out-of-vocabulary (see Remaining).

5. **Test suite inexpressible** (gap 5) → fully ported:
   - googletest: `[dev-dependencies]` with upstream's own bundled pin
     (`v1.15.2`, `find-package = "GTest"`). Locked eagerly (in
     `CppPkg.lock`), fetched/built only by `cpp-pkg test` — a library-only
     `cpp-pkg build` does no store work for it (verified).
   - `output_test_helper`: `dev = true` static library (the "output-checked
     tests" wave 1 wrote off are self-verifying binaries; they ported as
     ordinary test targets — nothing remained out of scope there).
   - 49 `test = true` executables, 84 `[[run]]` entries transcribed 1:1 from
     upstream's CTest registrations, including the 36-invocation
     `filter_test` matrix with positional expect-count args and empty-string
     `--benchmark_filter=` args.
   - `-UNDEBUG` rides in private `cxx-flags` (no schema home for an
     un-define — draws the documented lint warning, works via last-wins over
     the profile's `-DNDEBUG`), plus
     `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS`.
   - Result: `cpp-pkg test` 84 passed / 0 failed; reference `ctest` on the
     same checkout: 84 tests, 100% pass. Filters and the
     no-match hard error behave as spec'd.

6. **No install/export** (gap 6) → `install = true` (via `[target-defaults]`
   eligibility: fills exactly `benchmark` + `benchmark_main`, skips all 50
   dev/test targets), `[export] cmake-name = namespace = "benchmark"`
   (upstream's names). Headers derive from `includes.public`. The wave-1
   fixpoint test passes: plain CMake `find_package(benchmark 1.9.5)` against
   the cpp-pkg-installed prefix builds and runs a consumer reporting
   `v1.9.5` (SameMajorVersion ConfigVersion honored).

7. **No way to say "system threads"** (gap 7) → `Threads::Threads` builtin
   (ladder step 0): declared private on `benchmark` and on every test target
   upstream links `${CMAKE_THREAD_LIBS_INIT}` into. No-op on macOS by
   definition; `-pthread` on Linux comes with the S5 run.

8. **Schema ergonomics** (gap 8) → profile-stanza duplication gone
   (`[flags]`); `cxx-std = 17` and the large-file defines stated once in
   `[target-defaults]` (with `cxx11_test` overriding the scalar, as spec'd).

## Remaining

1. **Directory-scoped test facts repeat per target** (minor, ergonomics).
   Upstream states `-Wno-unused-variable`, `-UNDEBUG`,
   `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS` and (on clang)
   `HAVE_THREAD_SAFETY_ATTRIBUTES` once for the `test/` directory; the
   manifest repeats them in all 50 dev-target stanzas (~200 lines of the
   906) because `[target-defaults]` excludes flag keys (reserved, by design)
   and `[target-defaults.cfg.*]` is reserved. Not wrong — the manifest is
   generated-then-committed — but this is the single largest source of bulk,
   and any project with a test-dir flag policy will meet it. A scoped
   defaults mechanism (or unreserving flag keys in defaults) would collapse
   it.

2. **Global compiler-conditional defines have no single home** (minor).
   `HAVE_THREAD_SAFETY_ATTRIBUTES` is global upstream
   (`add_compile_definitions`) but must be spelled per target here since
   `[target-defaults.cfg.clang]` is reserved. Same shape as (1); recorded
   separately because it is `cfg`-specific: the first project needing a
   *platform*-conditional global define (this one is compiler-conditional)
   hits it too.

3. **Profile-conditional test defines inexpressible** (minor, correctness at
   the margin). Upstream scrubs `-DNDEBUG` and defines
   `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS` only for non-Debug configs.
   The manifest transcribes the release branch; a `--config debug` test run
   still gets `-UNDEBUG` (harmless — nothing to undefine) but wrongly keeps
   `TEST_BENCHMARK_LIBRARY_HAS_NO_ASSERTIONS`. `[profiles.*.cfg.*]` is
   reserved and cfg has no profile axis, deliberately. Debug-config test
   runs of this port are therefore slightly off-upstream (assertion-related
   expectations in `donotoptimize_test`/`diagnostics_test` could diverge).

4. **Exporting a header-less companion library trips the empty-derivation
   error** (minor, new-feature finding). `benchmark_main` has no headers of
   its own; `install = true` hard-errored ("header derivation is empty")
   until it was given `includes = { public = ["include"] }`. That spelling
   is defensible here — upstream's `install(TARGETS ... INCLUDES DESTINATION
   include)` puts `include/` on both exported targets' interfaces, and the
   derived headers dedupe byte-equal — but a companion library whose
   interface is purely "link me" (its headers all owned by a dep) has no
   honest spelling: the error's third suggestion ("remove install = true")
   would un-ship it. An explicit empty override
   (`public-headers = { patterns = [] }`) is currently a schema error too.
   Worth a ruling: header-less exported libraries are real (every
   `foo_main`-style lib).

5. **`-UNDEBUG` lint noise** (cosmetic). The documented
   `cxx-flags contains '-UNDEBUG'` warning fires once per test target — 50
   times per build. Correct per spec (warn, don't error), but at this
   multiplicity a once-per-flag summary would be kinder.

6. **Dependency-mode version trap half-remains** (recorded, unchanged).
   Source-mode version stamping is fixed by interpolation, but a *consumer*
   pinning benchmark at a non-tag `rev` still gets whatever
   `project(VERSION)` says (the store checkout has no `.git`, upstream's
   `git describe` fails silently). `${pin.self.requested}` would let a
   future CppPkg-native dep mode fix this; reserved today.

7. **Recorded losses, unchanged and deliberate**: `.pc` file emission
   (upstream ships `benchmark.pc`/`benchmark_main.pc` with a derived
   `Libs.private`), python tools (`share/googlebenchmark/tools`) and docs
   installs; Solaris `kstat` (out of cfg vocabulary); assembly tests
   (upstream gates them off on this machine anyway); per-case gtest
   discovery (`gtest_discover_tests` has no analogue — each gtest binary is
   one invocation, same as upstream's `add_gtest` here, so no fidelity lost
   for benchmark specifically).

## Verification summary (macOS arm64, AppleClang 21)

- Objects bit-identical to reference (5 TUs incl. version-embedding
  `benchmark.cc.o`); `nm -g` archive symbol lists identical (811 symbols).
- `cpp-pkg test`: 84/84 pass; reference `ctest`: 84/84 pass; invocation
  lists match 1:1.
- `cpp-pkg install`: 7 files; plain-CMake fixpoint consumer green.
- Second build/test/consumer-build: full no-ops (store + ninja cache hits).
- Linux/Windows branches written from upstream logic, validated here,
  pending S5 execution.
