# Migration: abseil-cpp (LTS 20260526.0)

Upstream: https://github.com/abseil/abseil-cpp
Ref: tag `20260526.0` = commit `5650e9cf76d3be4318d5fa3af38ee483ddfd5e4a`
(latest LTS at time of writing; verified via `git ls-remote --tags`).

Abseil is the target-count ergonomics candidate: 256 `absl_cc_library` calls
(216 non-test) plus 241 `absl_cc_test` calls, all built through a Bazel-like
CMake function layer (`CMake/AbseilHelpers.cmake`). Two experiments:

1. **Native port** (`CppPkg.toml`): rebuild a bottom-up subset of abseil with
   cpp-pkg as the build system — the transitive closure of
   `strings` + `str_format` + `flat_hash_map` (93 library targets, 54 of them
   header-only) plus a `demo` executable using `absl::StrCat`,
   `absl::StrFormat`, `absl::flat_hash_map`. Per-target deps are mined
   faithfully from upstream's `absl_cc_library` calls by `gen_toml.py`
   (~150-line Python parser/generator; the TOML was NOT hand-written — that
   is itself the finding, see GAPS.md).
2. **Consumer** (`consumer/`): abseil declared as an ordinary cpp-pkg
   dependency, exercising the tier-2 `find_package` probe at scale
   (217 extracted components).

## Reproduce

```sh
cd <workdir>
sh /opt/claude/cpp-pkg/migrations/abseil/pin.sh   # clones + overlays + patches
export CPPKG_STORE=<somewhere>

# native port
( cd upstream && cpp-pkg build && ./build/demo )

# consumer (dep = locally patched clone; see patches/ and GAPS.md)
( cd consumer && cpp-pkg build && ./build/demo )
```

`pin.sh` never edits upstream sources for the native port — it only ADDS
`CppPkg.toml`, `cppkg_stub.cc`, and `demo/main.cpp`. For the consumer it
prepares `absl-patched/`, a local clone with
`patches/0001-remove-absl-strings-self-dep.patch` committed and tagged
`20260526.0-cppkg1`, because cpp-pkg rejects upstream's `absl::strings`
self-link edge as a dependency cycle and has no dependency-patching
mechanism.

Regenerating the manifest: `python3 gen_toml.py <upstream-root>` emits the
generated target blocks; `CppPkg.toml` = `header.toml` + that output.

## Files

- `CppPkg.toml` — native-port manifest (684 lines; 94 targets). Source of
  truth; everything below the marker line is generator output.
- `header.toml` — hand-written prologue (package, profile link-flags for
  CoreFoundation, demo target).
- `gen_toml.py` — mines `absl_cc_library` calls, computes the closure, emits
  target blocks; documents every unexpressible construct as a
  `# UNEXPRESSIBLE` comment in the output.
- `overlay/` — files added to the upstream checkout (demo main, stub TU for
  header-only targets).
- `consumer/` — the dependency-consumption project
  (`@ABSL_PATCHED_REPO@` substituted by pin.sh).
- `patches/0001-remove-absl-strings-self-dep.patch` — drops the one
  self-dependency in upstream (`absl::strings` lists itself in its own DEPS;
  CMake tolerates the self-edge, cpp-pkg does not). Needed for the consumer
  path only; the generator strips it for the native port.
- `GAPS.md` — findings keyed to the design questions.

## Parity evidence

Protocol: byte-compare demo stdout across three builds of the same
`main.cpp`.

1. cpp-pkg native port (`upstream/build/demo`)
2. upstream CMake reference: abseil built+installed with its own CMake
   (`-DCMAKE_BUILD_TYPE=Release -DABSL_ENABLE_INSTALL=ON
   -DABSL_PROPAGATE_CXX_STD=ON -DCMAKE_CXX_STANDARD=17`), demo built via
   `find_package(absl)` against that install
3. cpp-pkg consumer (`consumer/build/demo`, abseil via store)

All three outputs are identical (`diff` clean; 10 lines, includes
`StrFormat` float/hex/width formatting and sorted `flat_hash_map` contents).
Verified twice: once in the authoring workdir, once from a fresh `pin.sh`
run (`NATIVE-REPRO-OK` / `CONSUMER-PARITY-OK` / `CONSUMER-REPRO-OK`).

Build health: native port = 248 ninja actions, ~3.5 s wall from clean
(Apple clang 21, arm64). Consumer dep build (all 216 abseil libs + probe +
install to store) ≈ 10 s wall. Second builds: `ninja: no work to do`, and
the consumer's second build is a store cache hit (no
"building dependency absl" line).

## Scope honesty

- Subset, not full abseil: 93/216 non-test library targets (the closure the
  demo needs). No tests ported (cpp-pkg has no test story — see GAPS.md);
  no `random`/`log`/`flags`/`status` closures; no DLL mode.
- macOS-only: the generator resolves upstream's platform-conditional
  LINKOPTS/DEPS (`$<$<PLATFORM_ID:...>>`, `LIBRT`, MinGW libs) for macOS by
  dropping them and hoisting CoreFoundation into profile link-flags. The
  upstream CMake build is portable; this port is not. That asymmetry is a
  finding (conditional-sources), not an accident.
- Header-only targets are modeled as static libraries over an empty stub TU
  (no interface-library kind in v0); their archives are empty but
  propagation is faithful.
