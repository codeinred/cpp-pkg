# Gaps — abseil-cpp 20260526.0, wave-2 edition (S4 re-migration)

Wave-1 findings re-audited against the wave-1 extensions. "Dissolved" maps
each wave-1 workaround to the feature that killed it, in real syntax.
"Remaining" is honest: it includes NEW bugs found in the wave-1 features
themselves. Severity: blocker / major / minor as before.

## Dissolved (workaround → feature)

1. **`file://` patched clone for the consumer → nothing at all.**
   Wave 1: upstream's `absl::strings → absl::strings` self-edge hard-errored
   as a cycle; the only out was pin.sh's locally cloned+patched+tagged repo
   (per-machine commit sha ⇒ per-machine config hash ⇒ unshareable store,
   uncommittable lockfile). Wave 2: self-link edges in extracted manifests
   are deduped tool-side (tool-fix batch #2), so the consumer needs **no
   patch and no `patches = [...]` either** — the dep is upstream truth:
   ```toml
   [dependencies.abseil]
   git = "https://github.com/abseil/abseil-cpp"
   tag = "20260526.0"
   find-package = "absl"
   ```
   Verified: lockfile pins the real commit `5650e9cf…`; a second checkout
   rebuilds from the store with zero "building dependency" lines (the exact
   sharing the wave-1 workaround destroyed). `patches/` and the pin.sh
   clone/sed machinery are deleted.

2. **Dep key ≡ CMake package name → `find-package`.** Wave 1 forced the key
   `absl` because the probe ran `find_package(<depkey>)`. The key is now
   `abseil` with `find-package = "absl"` (documented in tool-fix batch #4).

3. **COPTS dropped wholesale → `[flags.cfg.clang]` / `[flags.cfg.gcc]`.**
   Upstream's per-compiler split (`ABSL_LLVM_FLAGS` / `ABSL_GCC_FLAGS`) is
   transcribed 1:1 in header.toml (~40 flags clang, 15 gcc); per-test
   deltas (`ABSL_*_TEST_FLAGS` minus base — last-wins layering makes the
   trailing `-Wno-*` entries exact) are emitted per test target under
   `[targets.<t>.cfg.clang/gcc] cxx-flags`. The whole 88-test suite and the
   93-lib build compile with **zero warnings** under Apple clang 21 — the
   "build it the way its authors intended" gap is closed on this platform.

4. **CoreFoundation hoisted into `[profiles.release] link-flags` →
   `[targets.time_zone.cfg.macos]`.**
   ```toml
   [targets.time_zone.cfg.macos]
   link-flags = { public = ["-Wl,-framework,CoreFoundation"] }  # transcribed: $<$<PLATFORM_ID:Darwin,...>:...>
   ```
   Target-scoped (no longer sprayed on every executable) and
   config-independent (no longer silently lost under `--config debug`).
   The `public` spelling is a deliberate workaround for Remaining #1.

5. **Dropped Linux/system link inputs → cfg + builtin.** Wave 1's port was
   a macOS projection: `Threads::Threads` and `$<$<BOOL:${LIBRT}>:-lrt>`
   were dropped ("nowhere to put it"). Now: `Threads::Threads` is emitted
   verbatim as a dependency (builtin pseudo-package; 5 libraries + the
   tests that list it), and base carries
   `[targets.base.cfg.linux] link-flags = { public = ["-lrt"] }`
   (`# transcribed:` comment names the genexpr). Written from upstream's
   build logic; S5 executes them on Linux.

6. **29% generated repetition → `[target-defaults]`.** `cxx-std = 17` +
   `includes = { public = ["."] }` × 93 (and the `install`/`public-headers`
   × 93 an export would have added) collapse into one 8-line table.
   Measured: **660 → 470 generated lines** for the same 93 library targets
   (7.1 → 5.05 lines/target, −29% — the wave-1 prediction, exactly).

7. **Testing story ("full port impossible until it exists") → dev/test
   markers + `[dev-dependencies]` + `cpp-pkg test`.** The generator emits
   `dev = true` 1:1 from TESTONLY (21 libs in subset) and `test = true` per
   `absl_cc_test` (88 executables whose closure lies inside the subset).
   googletest v1.17.0 is a dev-dep (`find-package = "GTest"`), locked
   eagerly, built lazily — `cpp-pkg build` stays 249 actions and touches no
   gtest. `cpp-pkg test --jobs 8`: **88 passed, 0 failed**; filters and
   `-- --gtest_filter=...` passthrough work. Zero `[[run]]` entries needed
   (gtest binaries take the default invocation) — the zero-entry default is
   the right spelling here.

8. **Install/export cul-de-sac → `[export]` + `install = true` +
   `public-headers` override.** Written once each:
   `cmake-name = "absl"`, `namespace = "absl"`, default `install = true`
   (eligibility auto-skips the 21 dev libs and 88 tests — zero per-target
   lines), and the one total override for abseil's repo-root layout:
   `public-headers = { base = ".", patterns = ["absl/**/*.h", "absl/**/*.inc"] }`.
   `cpp-pkg install --prefix` stages 504 files; header set byte-matches the
   reference CMake install (384 `.h` + 24 `.inc`); a plain CMake consumer
   builds against OUR `abslConfig.cmake` with byte-identical demo output
   (ROUNDTRIP-PARITY-OK). The port is a producer now.

9. **Probe's unevaluated-genexpr notes (replayed on every cache hit) →
   gone.** Tool-fix #8: `$<BOOL:...>` inside LINK_ONLY is evaluated; the
   consumer build log carries no "unhandled generator expression" notes.

10. **`.cc`-smuggled-through-HDRS (latent wave-1 bug, found by the test
    suite).** Upstream lists `strings/internal/escaping.cc` under **HDRS**;
    CMake compiles it anyway. Wave 1 never noticed (demo doesn't call
    base64), the test suite linked and failed. Not a schema gap — the
    generator now routes compilable HDRS entries to sources (same quirk
    class as vtz's date `.cc`-in-INTERFACE_SOURCES, which got tool-fix #3
    for the extracted side). Native default build is 249 actions (+1).

## Remaining

### 1. NEW BUG (wave-1 feature): install/export drops private link-flags of static libraries — major

Spec §1.3: "`link-flags` on a static library propagate link-only;
consequence: public≡private for static-library `link-flags`". The bare-list
spelling (all-private sugar) honors this in-project — the native demo links
CoreFoundation fine — but **`cpp-pkg install` emission only carries the
public bucket** (`graph.rs` `public_link_flags = eff.link_flags.public` →
`shim.rs` `link_options`). With
`[targets.time_zone.cfg.macos] link-flags = ["-Wl,-framework,CoreFoundation"]`
the emitted `abslConfig.cmake` has no INTERFACE_LINK_OPTIONS, and an
external consumer dies at link:
`Undefined symbols: _CFTimeZoneGetName ... in libtime_zone.a`.
The documented equivalence breaks exactly at the export boundary, silently,
on the flag class (`-l`/`-framework` words) the interleaving rule exists
for. Workaround (this port, generator comment marks it): spell them
`link-flags = { public = [...] }`. Fix belongs in the tool: for
static-library targets, fold private link-flags into the exported
link-only channel (or reject the private spelling on installed static libs
so the manifest can't lie).

### 2. Header-only targets still compile a stub TU — major (scheduled: B10, wave 2)

54 of 93 library targets are `static-library` over `cppkg_stub.cc`.
Unchanged from wave 1, and now *worse-shaped*: the 54 empty archives are
also **installed** into the prefix and imported as STATIC by the emitted
Config (upstream exports them as INTERFACE libraries). Consumers link ~54
empty archives harmlessly, but the modeling lie is now visible outside the
build tree. interface-library is the named wave-2 item; this port is its
first customer.

### 3. NEW ergonomics headline: per-test COPTS deltas are the next 29% — major

Upstream has *two* flag environments: ABSL_DEFAULT_COPTS (all libs) and
ABSL_TEST_COPTS (all tests). `[flags]` expresses the first; nothing
expresses the second, so the generator emits the same 2 cfg blocks
(~17 clang flags + 7 gcc flags) into **every one of the 88 test targets**:
~350 lines, ~35% of the dev/test section — the same shape
`[target-defaults]` just killed for libraries. No current feature fits:
flag keys are reserved-out of `[target-defaults]` (pointing at `[flags]`),
`[flags]` cannot be scoped to test targets, and there are no named
reusable flag groups. Smallest fix consistent with the schema: a
dev/test-scoped refinement of the package flags layer
(e.g. `[flags.test.cfg.clang]`) — it is an environment statement, exactly
like `[flags]`, and upstream Bazel/CMake both model it that way.

### 4. Windows-only LINKOPTS remain comments — minor

`$<$<BOOL:${MINGW}>:-ladvapi32>` (base), `-ldbghelp` (symbolize): the cfg
vocabulary's `windows` atom cannot distinguish MinGW from MSVC, and these
are MinGW spellings. Left as `# not transcribed` comments (3 in the
manifest). Out of scope until a Windows toolchain exists; recorded.
`$<$<BOOL:${EXECINFO_LIBRARY}>:...>` (stacktrace) is genuinely empty on
all current platforms (glibc has backtrace in libc; macOS in libSystem;
musl lacks it entirely) — comment is the honest transcription.

### 5. Native self-edges still error — minor

Tool-fix #2 dedupes self-edges in *extracted* manifests only; a project
target listing itself still errors
(`dependency cycle in link closure: strings -> strings`, verified). So the
generator keeps stripping upstream's `absl::strings` self-dep for the
native port. Defensible (project manifests are user-authored), but the
asymmetry means a mechanical CMake→CppPkg converter must special-case what
the extractor now tolerates.

### 6. Still-standing wave-1 ergonomics residue — minor

- `type = "static-library"` × 114: `type` is deliberately excluded from
  `[target-defaults]`; accepted, but it is now the largest per-target
  constant left.
- TOML still has no include mechanism: generated blocks and the
  hand-written prologue share one 1548-line file with a marker comment
  (`targets-from` remains deferred).
- The generator itself remains the on-ramp at this scale — unchanged
  verdict; wave-1 features made its *output* reviewable, not unnecessary.

### 7. Scope: 88/241 tests, 93/216 libs — not a gap, a boundary

The 153 unported tests need closures this subset doesn't carry
(`random`/`log`/`flags`/`status`, benchmark-dependent tests). Nothing
schema-shaped blocks them anymore: extending ROOTS in gen_toml.py is the
whole job. The honest wave-1 extrapolation ("a full abseil port is
impossible until [testing] exists") is dead; a full port is now generator
elbow grease.

### 8. Untested-by-this-machine: the Linux branches — S5's job

`cfg.linux` `-lrt`, the Threads builtin's `-pthread` expansion, gcc's view
of the `[flags.cfg.gcc]` set and of the per-test deltas (`-Wno-*` flags gcc
only diagnoses when another warning fires) are transcribed from upstream
logic but executed only on macOS here. Wrong guesses are S5 findings;
absent branches would have been S4 failures — none are absent.
