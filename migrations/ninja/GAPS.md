# Gaps — ninja v1.13.2, wave-2 edition (post wave-1 re-migration)

Wave-1 found eight gaps. The wave-1 extensions dissolve five of them outright
and most of a sixth; this re-migration also found **one new bug in a wave-1
feature** (install of an executable — item R1), which is the only thing
keeping the port off full green.

## Dissolved (workaround → feature)

### D1. Posix-only projection → `cfg` sub-tables (was: conditional-sources)

The wave-1 manifest hard-wired the posix source set and declared itself
macOS/Linux-only. Now upstream's `if(WIN32)/else()` block is written as
declared branches:

- `[targets.libninja.cfg.unix]` — `jobserver-posix.cc`, `subprocess-posix.cc`
- `[targets.libninja.cfg.windows]` — the seven win32 sources + `NOMINMAX`
- `[targets.libninja.cfg.msvc]` — `_CRT_SECURE_NO_WARNINGS`
- `[targets.ninja_test.cfg.windows]` — `includes_normalize_test.cc`,
  `msvc_helper_test.cc`; `[targets.ninja_test.cfg.msvc]` —
  `_CRT_NONSTDC_NO_DEPRECATE`

Windows groups are validated-never-expanded on this machine, as specified.
The Linux branch is written from upstream's own logic for S5 validation.

### D2. Configure-check answer for ppoll → labeled `cfg.linux` transcription
(was: conditional-sources / configure checks, the "subtly wrong Linux
binary" item)

`check_cxx_symbol_exists(ppoll poll.h)` is true on Linux, false on macOS.
`[targets.libninja.cfg.linux] defines = { public = ["USE_PPOLL=1"] }` with
the normative `# transcribed:` comment. Wave-1's silent pselect fallback on
Linux is dead. (Public, not private, because upstream's define is
directory-global and `subprocess_test.cc` in ninja_test branches on it; the
public edge reaches exactly the TUs that can see `subprocess.h`.)

### D3. pin.sh pre-generated browse_py.h → `[generate.browse-py-h]`
(was: codegen-escape-hatch, the worked-around half)

Tier-b command step (`sh src/inline.sh kBrowsePy`, `stdin = src/browse.py`,
`stdout = build/browse_py.h`); the ninja target consumes it via
`includes = { private = ["${gen}"] }`. Verified: output byte-identical to
the upstream recipe, and the edge **reruns when browse.py is touched**
(restat prunes downstream on unchanged bytes) — the silent-staleness defect
of pin.sh-time generation is gone. The pin.sh codegen block is deleted.

### D4. re2c "dodged" → two `checked-in` steps + `gen --check`
(was: codegen-escape-hatch, the dodged half)

`[generate.depfile-parser]` / `[generate.lexer]` carry upstream's exact re2c
argv (minus `-o`; the runner captures stdout) with
`checked-in = "src/depfile_parser.cc"` / `"src/lexer.cc"`. `cpp-pkg build`
compiles the committed files with no questions asked — exactly upstream's
no-re2c fallback; `cpp-pkg gen --check` is the drift guard for machines
with re2c. Verified end-to-end with a stub re2c reproducing the committed
bytes: `gen --check` reports both files current; `gen` no-ops without
touching the tree.

### D5. Unconditional googletest + prose test protocol → dev/test markers +
runner (was: testing-story)

- `googletest` moved to `[dev-dependencies]`; **`[dependencies]` is empty
  again** — the manifest matches upstream's "zero dependencies" pitch.
  Verified lazy: after `cpp-pkg build` of a fresh store, `pkg/` contains
  nothing; the lockfile still eagerly pins googletest
  (`6910c9d9…`, same commit as wave 1).
- `ninja_test` is `test = true` with a `[[run]]` entry
  (`cwd = "build/ninja_test-scratch"`, auto-created — the README's
  "run from a writable scratch cwd" prose is now machine-checked).
  `cpp-pkg test` builds gtest + ninja_test and runs the suite:
  **409/409 from 31 suites**, same as wave 1's manual invocation.
- The seven perftests are `dev = true` (not `test` — upstream never
  `add_test()`s them; the label would lie). They leave the default build
  and stay buildable by name (`cpp-pkg build canon_perftest` verified).
- `Threads::Threads` is now a declared edge on ninja_test (wave-1 builtin),
  matching upstream's explicit `find_package(Threads REQUIRED)` — no-op on
  macOS, `-pthread` on Linux for free. The wave-1 attribution accident
  (Threads owned by whichever probe mentioned it) is moot: builtins resolve
  at ladder step 0 and cannot be shadowed.

### D6. 4× duplicated profile stanzas → `[flags.cfg.clang]` + `[flags.cfg.gcc]`
(was: per-target-flags / schema-ergonomics)

Upstream's global `-Wno-deprecated` (applied for every non-MSVC compiler) is
now two one-line cfg groups on `[flags]` — every target, every profile;
verified present on all 33 TUs in compile_commands.json, build warning-clean.
Two lines instead of the wave-1 doc's promised one because v1 has no
`not(msvc)` combinator (see R7). Upstream's MSVC `/W4 /wd…` set is
transcribed under `[flags.cfg.msvc]` for the day a Windows toolchain exists.

### D7. cxx-std repeated per target → `[target-defaults]`

`cxx-std = 11` stated once; `ninja_test` overrides to 14 (gtest 1.16 floor)
via scalar fill-if-absent. Eleven repetitions deleted.

### D8 (partial). `install(TARGETS ninja)` → `install = true`

The declaration is now expressible and present on the one product target.
Execution is blocked by a new bug — see R1.

## Remaining

### R1. NEW BUG (blocker for the install story): `cpp-pkg install` of an
executable with local static-library deps is impossible

`install = true` on `ninja` (an executable depending on `libninja`,
`libninja-re2c`) fails:

```
error: target 'libninja' is exported but its header derivation is empty
(public include dirs: []) — add public headers, a public-headers override,
or remove install = true
```

Both `cpp-pkg install --prefix P` and `cpp-pkg install ninja --prefix P`
fail identically (also under `--list`). Root cause (read, not patched —
src/ is off-limits to this migration): `plan_install` in
`/opt/claude/cpp-pkg/src/shim.rs` (~line 865) unconditionally closes the
selection over local dependency edges and then stages every closure member
by its own kind — so the static libs are treated as *exported libraries*
(archive to `lib/`, headers derived) and `derive_headers` hard-errors,
since libninja legitimately has no public include dirs. This contradicts
the validator's own documented rule a hundred lines earlier
(`validate_exported_closure`, shim.rs ~562: "Executables statically link
their closure and impose no such rule") — validation passes, planning then
reintroduces the rule it waived. Consequence: the spec's "bin/ninja exists
at last" (wave1-extensions §6/§10) does not hold for exactly the shape
ninja has — an exe over unexported internal static libs, which is also
json-tui's and cppcheck's shape. No clean manifest workaround exists:
`install = true` + `public-headers` on the libs would stage archives and
headers upstream never installs. The manifest keeps the correct
declaration; it will work when the closure loop is gated on library roots.

### R2. Windows remains declared, not validated (major, expected)

No Windows toolchain exists; `windows`/`msvc` groups are
accepted-but-false. Additionally two Windows details are inexpressible even
declaratively: `windows/ninja.manifest` as an executable source (the
extension table rejects non-source files — needs a `frameworks`-like
carrier for linker inputs), and `getopt.c` compiled as C++
(`set_source_files_properties LANGUAGE CXX`; the extension table is a hard
rule). Both recorded in cfg.windows comments; moot until a toolchain lands.

### R3. AIX/OS400 branch out of vocabulary (minor, deliberate)

`-lperfstat`, `__STDC_FORMAT_MACROS`, getopt-as-C++ have no `aix` atom.
Per wave-1 §10 this is the accepted remainder.

### R4. Per-source properties still absent (minor)

`NINJA_PYTHON`/`NINJA_HAVE_BROWSE` + the `${gen}` include are per-source
on `browse.cc` upstream, target-wide here (harmless on a two-file target,
unsound in general — unchanged from wave 1).

### R5. IPO/LTO knob still absent (minor)

Upstream enables IPO for Release when supported; no `lto = true` profile
switch exists. Binary-size delta persists (~397 KB vs ~348 KB), behavior
identical.

### R6. Package-scope conditional *defines* have no first-class home (minor,
design note)

`add_compile_definitions(NOMINMAX)` / `(USE_PPOLL=1)` are environment
statements, but `[flags]` has only flag keys (a `-DNOMINMAX` entry would
draw the "use `defines`" lint) and `[target-defaults].cfg.*` is reserved.
The port expresses them as **public defines on libninja**, which works
because every executable consumes libninja — but it is interface phrasing
for an environment statement. A `defines` key on `[flags]`/`[flags.cfg.*]`
would say it directly.

### R7. No `not(msvc)` / combinators (minor, reserved as designed)

Upstream's "every non-MSVC compiler" becomes duplicated `cfg.clang` +
`cfg.gcc` groups. Fine at this size; the reserved quoted-key combinator
spelling would collapse them.

### R8. `gen` with a missing tool errors rawly (minor, polish)

With re2c absent, `cpp-pkg gen --check` prints
`sandbox-exec: execvp() of 're2c' failed: No such file or directory` and
`command … failed (exit 71)`. Correct behavior (the tool is genuinely
required for the *gen* verb; build is unaffected), but a not-found hint
naming the tool — like the sysdep "install it" error — would be kinder.

### R9. OBJECT libraries / whole-archive (minor here; B11, unchanged)

Both libs remain static archives; observationally equivalent for ninja
(409/409), still a latent wrong-behavior trap for self-registering TUs.

Cosmetic, recorded only: upstream's `-fdiagnostics-color` /
`CMAKE_COLOR_DIAGNOSTICS` handling is not reproduced.

## Verification log (2026-08-14, fresh store `store-s4-ninja`)

- `cpp-pkg build`: 37 edges, warning-clean; **no googletest work** (store
  `pkg/` empty afterward); lockfile eagerly pins googletest anyway.
- `./build/ninja --version` → `1.13.2`; `-t list` → full subtool list
  including `browse` (generate edge reached the binary);
  `build/gen/build/browse_py.h` byte-identical to the upstream recipe.
- `cpp-pkg test` → `NinjaTest … ok, 1 passed`; `--verbose` shows
  **409 tests from 31 suites, all passed** (231 ms).
- Cache: `rm -rf build && cpp-pkg test` rebuilt project TUs only —
  googletest served from store (`googletest-7c9ce38684d2a5bf30da5c28e7b0cd0a`,
  same key as wave 1: the consumer-only `[flags.cfg.*]` warning flag did
  not invalidate it). Follow-up `build`/`test`/`build` alternation: zero
  edges re-run.
- `touch src/browse.py && cpp-pkg build` → 1 GEN edge, restat stops there.
- Stub-re2c `gen --check` / `gen`: both report current, tree untouched.
- `cpp-pkg install` (both forms, and `--list`): fails per R1.
