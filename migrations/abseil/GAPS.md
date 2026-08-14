# Gaps found migrating abseil-cpp 20260526.0

Each item tagged with the design-question key. Severity: blocker = stops the
migration without a workaround; major = real friction, workaround exists;
minor = cosmetic/latent.

## schema-ergonomics — the target-count question (headline finding)

**Numbers.** Demo closure = 93 library targets, 660 generated TOML lines
(7.1 lines/target). Full abseil = 216 non-test libraries + 40 TESTONLY
libraries + 241 test executables. Extrapolation: ~1,550 TOML lines for all
non-test libraries; ~4,300+ lines with tests. Hand-writing is out of the
question; hand-*maintaining* across LTS bumps doubly so (upstream reshuffles
internal targets every release — e.g. this LTS added
`strings_append_and_overwrite`, `iterator_traits_internal`).

**What actually worked: a generator.** `gen_toml.py` (~150 lines of Python)
parses `absl_cc_library` calls and emits the TOML mechanically. Writing it +
debugging took well under an hour because abseil's CMake is Bazel-shaped.
Verdict on "schema sugar vs generator tool": **both, in different roles**:

- A converter tool is the only realistic on-ramp at this scale — but abseil
  is the *easy* case (structured macro calls). A general CMake→CppPkg
  converter would need the file-API/configure-probe route, not text parsing.
- Schema sugar attacks the 93× repetition the generator had to emit:
  `cxx-std = 17` and `includes = { public = ["."] }` appear identically in
  every one of the 93 targets (~190 lines, 29% of the generated text, is
  pure repetition). A `[target-defaults]` table (or per-directory defaults)
  would eliminate it and make even generated files reviewable.
- TOML has no include mechanism, so generated blocks and the hand-written
  prologue share one 684-line file with a "generated below this line"
  marker. A `targets-from = "..."` include (or a conventions like
  `CppPkg.d/*.toml`) would keep generator output out of the hand-edited
  file.
- Target globbing (the other idea in the question) would NOT have helped
  here: abseil's value is precisely its fine-grained dep graph; globbing
  sources into one blob discards it. Defaults + generator preserve it.

Severity: major (workaround: generator script, checked in).

## object-libraries / interface targets — no header-only target kind

54 of the 93 closure targets are header-only (`config`, `core_headers`,
`type_traits`, `span`, `optional`, `memory`, ...). v0 has no
`interface-library`, so each is a `static-library` compiling a shared empty
stub TU (`cppkg_stub.cc`), producing 54 empty `.a` files whose only job is
to exist as graph nodes. Propagation (public deps/includes/defines) works
faithfully through them, and the build cost is negligible — but it is a
modeling lie, it spams `ranlib: no symbols` warnings, and any tool that
inspects archives sees 54 empty libraries. The alternative workaround
(rewriting deps to skip header-only nodes) would have destroyed the faithful
graph. `interface-library` is the single schema addition that would have
removed the largest hack in this port. (Abseil's CMake does not use OBJECT
libraries outside DLL mode, so object-libraries proper went unexercised.)
Severity: major.

## per-target-flags

Two distinct sub-gaps:

1. **Compile flags.** Every abseil target sets `COPTS ${ABSL_DEFAULT_COPTS}`
   (a curated warning list, `-Wall -Wextra -Wcast-qual ...`). Targets have
   no `cxx-flags` field, so these are dropped wholesale. Harmless for
   artifact parity, fatal for "build this project the way its authors
   intended" (warnings-as-errors projects would diverge immediately).
2. **Link flags / frameworks.** `absl::time` carries
   `$<$<PLATFORM_ID:Darwin,...>:-Wl,-framework,CoreFoundation>` in LINKOPTS.
   No per-target link-flags field exists, so the framework rides on
   `[profiles.release] link-flags` — i.e. EVERY executable in the project
   links CoreFoundation because one library needs it, and the manifest under
   `--config debug` silently loses it (profile-scoped, not target-scoped).
   The extraction path handles this exact edge correctly for *imported*
   targets (frameworks bucket); native targets deserve the same field.

Severity: major.

## install-export — a library project cannot be a producer

The native port builds 93 static libraries but there is no way to
(a) install them + headers to a prefix, or (b) export them so *another*
cpp-pkg project could consume this port as a package (`absl::strings` etc.).
cpp-pkg today is consumer-only: the only way to package what we just built
is to throw the port away and consume upstream's CMake via extraction —
which is what the consumer experiment does. For the "replace the build
system Cargo-style" story, library authors are exactly the users who need
`cpp-pkg install` / an export manifest. Round-trip idea from the design
notes (emit CPS or Config shims for *local* targets) would close this.
Severity: major (by design in v0, but abseil makes it concrete: the port is
a cul-de-sac artifact-wise).

Sub-gap, consumption side: the tier-2 probe runs `find_package(<depkey>)`,
so the dep key MUST equal the CMake package name. `[dependencies.abseil]`
fails ("Could not find abseilConfig.cmake"); the key has to be `absl`.
`exposes-namespace` decouples target namespaces from the key, but nothing
decouples the *config-file name* from the key. Needs a `cmake-name`/
`config-name` field. Severity: major (workaround: rename the key; but a
project needing two deps whose config names collide with desired keys has
no out).

## dep-provisioning

1. **No dependency patching + real-world exports contain self-edges
   (blocker, workaround found).** Upstream ships
   `absl::strings → absl::strings` in its own DEPS; the installed
   `abslTargets.cmake` therefore has a self-edge in
   INTERFACE_LINK_LIBRARIES. CMake tolerates it; cpp-pkg hard-errors:
   `dependency cycle in link closure: absl::strings -> absl::strings`.
   Unpatched abseil 20260526.0 is thus **unusable as a cpp-pkg dependency**.
   Fix belongs in the tool (treat self-edges as no-ops — they are
   idempotent, not cyclic). The only user-side out was patching the dep,
   which cpp-pkg doesn't support either, so the workaround is a locally
   cloned+patched+tagged repo referenced by `file://` URL (pin.sh). That
   workaround has a nasty secondary cost: the patch commit's sha differs per
   machine/timestamp, so the dep's config hash differs per checkout
   (observed: `fee068f7...` vs `f4632513...`) — the store can never share
   these entries, and the lockfile can't be committed meaningfully. A
   first-class `patches = [...]` field (hash = base commit + patch bytes)
   would fix both. Severity: blocker (for unpatched consumption) / major
   (patch mechanism).
2. **System libs via find-module results.** `absl::base` deps on
   `Threads::Threads`; the native schema has no way to say "pthread". On
   macOS it's folded into libSystem so dropping it works; a Linux port would
   need it and has nowhere to put it (same for the `$<$<BOOL:${LIBRT}>:-lrt>`
   and `$<$<BOOL:${EXECINFO_LIBRARY}>:...>` LINKOPTS). Severity: minor on
   macOS, major the day Linux is a target.
3. Probe-side handling of those same constructs in the *installed* export is
   graceful: six `unhandled generator expression inside LINK_ONLY` notes
   (`-lrt`, `-ladvapi32`, `-llog`, `-lbcrypt`, `-ldbghelp`, EXECINFO), all
   correctly droppable on macOS since their conditions are false/NOTFOUND
   here. But the probe records the *unevaluated* text with baked-in
   configure-time values — on a platform where a condition is true the
   library would be silently dropped rather than linked. Severity: minor
   (macOS), latent-major (portability). Cosmetic sub-note: these notes are
   replayed on every build, including cache hits.

## conditional-sources

Abseil keeps source lists platform-unconditional (variation lives in
preprocessor guards), so the classic conditional-sources problem barely
appeared. Where conditionality does live — LINKOPTS/DEPS generator
expressions — the port had to bake in "macOS, Apple clang" at generation
time. Upstream's one CMakeLists is portable; our CppPkg.toml is a
platform-specific projection of it. Any schema answer (cfg()-style keys like
`[targets.time.macos]`, or genexpr-lite strings) must cover link
inputs/flags, not just sources — sources were never the problem in this
project. Severity: minor here, but this is the mildest possible test of the
question.

## testing-story

Not attempted, by scope — but the shape of the gap is measurable: 241
`absl_cc_test` executables + 40 TESTONLY libraries, all gated upstream on
`BUILD_TESTING AND ABSL_BUILD_TESTING`, all depending on GoogleTest
(provisioned upstream via `ABSL_USE_EXTERNAL_GOOGLETEST` or FetchContent).
Porting them needs: a test target kind (or `test = true`), test-only deps
that stay out of the normal graph, a gtest provisioning answer, and a
runner (`cpp-pkg test`). Nothing in v0 covers any of these; the honest
extrapolation is that a full abseil port is impossible until they exist.
Severity: major (scoped out here, so not a blocker for this migration).

## codegen-escape-hatch

Genuinely unexercised: abseil has no generated sources, no configure-time
file generation, no downloads. Zero data from this project; the question
needs a different migration (protobuf, ICU) to bite.

## Tool-bug observations (not schema gaps)

- Self-edge = cycle (above) is arguably a manifest-resolver bug, not a
  schema gap: CMake's own semantics dedupe self-links.
- The upstream self-dep also had to be stripped by the *generator* for the
  native port (`absl::strings` in its own deps would presumably trip the
  same cycle check on native targets).
