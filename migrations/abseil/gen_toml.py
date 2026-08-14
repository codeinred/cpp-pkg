#!/usr/bin/env python3
"""Generate CppPkg.toml targets for a subset of abseil-cpp (wave-2 edition).

Mines `absl_cc_library(...)` and `absl_cc_test(...)` calls out of
absl/**/CMakeLists.txt (they are structured and greppable, per upstream
convention), computes the transitive closure of a set of root targets, and
emits one [targets.<name>] block per library, plus dev/test targets for the
TESTONLY libraries and gtest executables whose dependency closure stays
inside the ported subset.

Wave-1 features used natively (wave-1 workarounds now gone):
  - [target-defaults] in header.toml carries cxx-std / includes / install /
    public-headers once; the generator no longer repeats them per target.
  - Upstream COPTS live in [flags.cfg.clang] / [flags.cfg.gcc] (header.toml,
    transcribed from absl/copts/GENERATED_AbseilCopts.cmake); per-test COPTS
    deltas (ABSL_TEST_COPTS minus ABSL_DEFAULT_COPTS) are emitted per test
    target under [targets.<t>.cfg.clang]/[.cfg.gcc] cxx-flags.
  - Platform-conditional LINKOPTS become cfg link-flags sub-tables
    (`# transcribed:` comments name the upstream generator expression);
    MinGW-only entries stay comments (no mingw predicate in the cfg
    vocabulary — cfg.windows would be wrong for MSVC).
  - Threads::Threads is emitted as-is (builtin pseudo-package since wave 1).
  - TESTONLY -> dev = true; absl_cc_test -> test = true (1:1), deps on
    GTest::* resolve via the googletest [dev-dependencies] entry.

Still encoded here (remaining gaps, see GAPS.md):
  - No interface-library kind yet (B10, wave 2): header-only targets remain
    static-library over the cppkg_stub.cc TU.
  - Upstream's absl::strings self-dep is stripped for native targets (the
    wave-1 tool fix dedupes self-edges in *extracted* manifests; a native
    target still may not list itself).

Usage: gen_toml.py <upstream-root> > targets.toml
"""
import re
import sys
from pathlib import Path

ROOTS = ["strings", "str_format", "flat_hash_map"]

# Builtin pseudo-packages (since wave 1): emitted verbatim as dependencies.
BUILTINS = {"Threads::Threads"}
# Dev-dep-exported targets legal in test/TESTONLY deps (googletest dev-dep).
GTEST = {"GTest::gtest", "GTest::gtest_main", "GTest::gmock",
         "GTest::gmock_main"}

# LINKOPTS generator expressions with a cfg transcription (since wave 1).
# Maps the upstream genexpr to (predicate, flag) — everything else is echoed
# as a comment.
LINKOPT_CFG = {
    "$<$<BOOL:${LIBRT}>:-lrt>": ("linux", "-lrt"),
    "$<$<PLATFORM_ID:Darwin,iOS,tvOS,visionOS,watchOS>:-Wl,-framework,CoreFoundation>":
        ("macos", "-Wl,-framework,CoreFoundation"),
}

KEYWORDS = {"NAME", "HDRS", "SRCS", "COPTS", "DEFINES", "LINKOPTS", "DEPS",
            "PUBLIC", "TESTONLY", "DISABLE_INSTALL"}


def parse_calls(root: Path, macro: str):
    """Yield parsed blocks for one absl_cc_* macro across absl/*/CMakeLists."""
    out = {}
    for cml in sorted(root.glob("absl/*/CMakeLists.txt")):
        text = cml.read_text()
        subdir = cml.parent.relative_to(root).as_posix()
        for m in re.finditer(macro + r"\((.*?)\n\)", text, re.S):
            lib = parse_block(m.group(1), subdir)
            if lib:
                if lib["name"] in out:
                    raise SystemExit(f"duplicate {macro} name: {lib['name']}")
                out[lib["name"]] = lib
    return out


def parse_block(body: str, subdir: str):
    tokens = []
    for raw in body.splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            tokens.extend(re.findall(r'"[^"]*"|\S+', line))
    lib = {"dir": subdir, "srcs": [], "hdrs": [], "deps": [], "defines": [],
           "linkopts": [], "copts": [], "public": False, "testonly": False}
    cur = None
    for tok in tokens:
        bare = tok.strip('"')
        if tok in KEYWORDS:
            if tok == "PUBLIC":
                lib["public"] = True
            elif tok == "TESTONLY":
                lib["testonly"] = True
            elif tok == "DISABLE_INSTALL":
                pass
            else:
                cur = tok.lower()
            continue
        if cur == "name":
            lib["name"] = bare
            cur = None
        elif cur == "srcs":
            if bare.endswith((".cc", ".c")):
                lib["srcs"].append(f"{subdir}/{bare}")
            elif not bare.endswith((".h", ".inc")):
                lib.setdefault("odd_srcs", []).append(bare)
        elif cur == "hdrs":
            # upstream quirk: strings_internal lists internal/escaping.cc
            # under HDRS; CMake compiles any .cc in the target's sources
            # regardless of which keyword it arrived through, so treat
            # compilable HDRS entries as sources (same class of quirk as
            # vtz's date .cc-in-INTERFACE_SOURCES, fixed tool-side for
            # extracted deps; project manifests stay strict, so the
            # generator routes it).
            if bare.endswith((".cc", ".c")):
                lib["srcs"].append(f"{subdir}/{bare}")
            else:
                lib["hdrs"].append(f"{subdir}/{bare}")
        elif cur == "deps":
            lib["deps"].append(bare)
        elif cur == "defines":
            lib["defines"].append(bare)
        elif cur == "linkopts":
            lib["linkopts"].append(bare)
        elif cur == "copts":
            lib["copts"].append(bare)
    return lib if "name" in lib else None


def closure(libs, roots):
    """Transitive closure of non-test libraries, dependency order."""
    seen, order = set(), []

    def visit(name):
        if name in seen:
            return
        seen.add(name)
        lib = libs.get(name)
        if lib is None:
            raise SystemExit(f"unknown target in closure: {name}")
        for d in lib["deps"]:
            if d.startswith("absl::") and d != f"absl::{name}":
                visit(d[len("absl::"):])
        order.append(name)

    for r in roots:
        visit(r)
    return order


def qualifying_dev(libs, tests, core):
    """TESTONLY libs and tests whose absl deps stay inside the ported set.

    A TESTONLY lib qualifies when every absl dep is in `core` or another
    qualifying TESTONLY lib; a test qualifies the same way, with GTest::* and
    Threads::Threads additionally allowed. Anything else (e.g. benchmark
    deps) disqualifies — the port only carries the closure it can build.
    """
    testonly = {k: v for k, v in libs.items() if v["testonly"]}
    dev = {}
    changed = True
    while changed:
        changed = False
        for name, lib in testonly.items():
            if name in dev:
                continue
            ok = True
            for d in lib["deps"]:
                if d.startswith("absl::"):
                    t = d[len("absl::"):]
                    if t != name and t not in core and t not in dev:
                        ok = False
                        break
                elif not d.startswith("$<") and d not in BUILTINS | GTEST:
                    ok = False
                    break
            if ok:
                dev[name] = lib
                changed = True

    picked_tests = {}
    for name, t in sorted(tests.items()):
        ok = True
        for d in t["deps"]:
            if d.startswith("absl::"):
                tt = d[len("absl::"):]
                if tt not in core and tt not in dev:
                    ok = False
                    break
            elif not d.startswith("$<") and d not in BUILTINS | GTEST:
                ok = False
                break
        if ok:
            if name in core or name in dev:
                raise SystemExit(f"test name collides with library: {name}")
            picked_tests[name] = t
    return dev, picked_tests


def parse_copts_lists(root: Path):
    """Parse the generated per-compiler flag lists out of upstream."""
    text = (root / "absl/copts/GENERATED_AbseilCopts.cmake").read_text()
    lists = {}
    for m in re.finditer(r"list\(APPEND (ABSL_\w+)\n(.*?)\n\)", text, re.S):
        lists[m.group(1)] = re.findall(r'"([^"]+)"', m.group(2))
    return lists


def test_flag_delta(lists, base_key, test_key):
    """ABSL_*_TEST_FLAGS minus ABSL_*_FLAGS, order preserved.

    Layering is last-wins (wave-1 contract), so appending the delta after the
    package-level [flags.cfg.*] base reproduces the test flag set: the
    trailing -Wno-* entries switch off base warnings the test set drops.
    -DNOMINMAX is skipped (windows-only; -D belongs in `defines` anyway).
    """
    base = set(lists[base_key])
    return [f for f in lists[test_key]
            if f not in base and not f.startswith(("-D", "/"))]


def dep_list(lib, own_name, allow_dev):
    """Returns (deps, linkopt_like) — upstream sometimes writes link flags
    directly in DEPS (time_zone's CoreFoundation genexpr); route those to the
    linkopts channel."""
    deps, linkopt_like = [], []
    for d in lib["deps"]:
        if d.startswith("$<"):
            linkopt_like.append(d)
        elif d.startswith("absl::"):
            t = d[len("absl::"):]
            # upstream quirk: absl::strings lists itself in its own DEPS
            # (CMake tolerates the self-alias edge). The wave-1 tool fix
            # dedupes self-edges in extracted dependency manifests; a native
            # target still cannot reference itself, so strip it here.
            if t != own_name:
                deps.append(t)
        elif d in BUILTINS:
            deps.append(d)          # builtin pseudo-package, no declaration
        elif allow_dev and d in GTEST:
            deps.append(d)          # dev-dep-exported target (googletest)
        else:
            raise SystemExit(f"unexpressible dep on {own_name}: {d}")
    return deps, linkopt_like


def fmt_list(items):
    return ", ".join(f'"{i}"' for i in items)


def emit_linkopts(out, name, linkopts):
    """cfg link-flags for known genexprs; comments for the rest."""
    by_pred = {}
    for lo in linkopts:
        if lo == "${ABSL_DEFAULT_LINKOPTS}":
            continue                # empty on every non-MSVC platform
        if lo in LINKOPT_CFG:
            pred, flag = LINKOPT_CFG[lo]
            by_pred.setdefault(pred, []).append((flag, lo))
        else:
            out.append(f"# not transcribed (MinGW/MSVC-only or out of cfg "
                       f"vocabulary): {lo}")
    for pred, entries in sorted(by_pred.items()):
        out.append(f"[targets.{name}.cfg.{pred}]")
        flags = fmt_list(e[0] for e in entries)
        srcs = "; ".join(e[1] for e in entries)
        # public spelling, deliberately: the spec makes link-flags on a
        # static library public≡private (both propagate link-only), but
        # install/export emission only carries the public bucket into
        # abslConfig.cmake — the bare-list (all-private) spelling silently
        # under-links external consumers (GAPS.md, wave-2 Remaining #1).
        out.append(f"link-flags = {{ public = [{flags}] }}  # transcribed: {srcs}")


def emit_lib(libs, name, dev=False):
    lib = libs[name]
    out = [f"[targets.{name}]"]
    out.append('type = "static-library"')
    if dev:
        out.append("dev = true")
    if lib["srcs"]:
        srcs = fmt_list(lib["srcs"])
    else:
        # header-only: still no interface-library kind (B10, wave 2); the
        # stub TU keeps the node in the graph so public deps/includes/
        # defines propagate exactly as upstream declared them.
        srcs = '"cppkg_stub.cc"'
    out.append(f"sources = [{srcs}]")
    if lib["defines"]:
        out.append(f"defines = {{ public = [{fmt_list(lib['defines'])}] }}")
    deps, linkopt_like = dep_list(lib, name, allow_dev=dev)
    if deps:
        out.append(f"dependencies = {{ public = [{fmt_list(deps)}] }}")
    emit_linkopts(out, name, lib["linkopts"] + linkopt_like)
    out.append("")
    return out


def emit_test(tests, name, deltas):
    t = tests[name]
    out = [f"[targets.{name}]"]
    out.append('type = "executable"')
    out.append("test = true")
    out.append(f"sources = [{fmt_list(t['srcs'])}]")
    if t["defines"]:
        out.append(f"defines = [{fmt_list(t['defines'])}]")
    deps, linkopt_like = dep_list(t, name, allow_dev=True)
    out.append(f"dependencies = [{fmt_list(deps)}]")
    for lo in t["linkopts"] + linkopt_like:
        if lo != "${ABSL_DEFAULT_LINKOPTS}":
            out.append(f"# not transcribed: {lo}")
    # ABSL_TEST_COPTS delta over ABSL_DEFAULT_COPTS, per compiler.
    for pred, key in (("clang", "llvm"), ("gcc", "gcc")):
        if deltas[key]:
            out.append(f"[targets.{name}.cfg.{pred}]")
            out.append(f"cxx-flags = [{fmt_list(deltas[key])}]")
    out.append("")
    return out


def main():
    root = Path(sys.argv[1])
    libs = parse_calls(root, "absl_cc_library")
    tests = parse_calls(root, "absl_cc_test")
    nontest = {k: v for k, v in libs.items() if not v["testonly"]}
    sys.stderr.write(f"parsed {len(libs)} absl_cc_library calls "
                     f"({len(nontest)} non-test), {len(tests)} absl_cc_test "
                     f"calls\n")

    core = closure(nontest, ROOTS)
    dev, picked = qualifying_dev(libs, tests, set(core))
    lists = parse_copts_lists(root)
    deltas = {
        "llvm": test_flag_delta(lists, "ABSL_LLVM_FLAGS",
                                "ABSL_LLVM_TEST_FLAGS"),
        "gcc": test_flag_delta(lists, "ABSL_GCC_FLAGS",
                               "ABSL_GCC_TEST_FLAGS"),
    }

    out = []
    n_iface = 0
    for name in core:
        if not libs[name]["srcs"]:
            n_iface += 1
        out += emit_lib(libs, name)
    lib_lines = len(out)

    out.append("# ---- dev/test targets (TESTONLY libs + absl_cc_test) ----")
    out.append("")
    for name in sorted(dev):
        out += emit_lib(libs, name, dev=True)
    for name in sorted(picked):
        out += emit_test(picked, name, deltas)

    sys.stderr.write(
        f"library targets: {len(core)} ({n_iface} header-only stubs), "
        f"{lib_lines} lines; dev libs: {len(dev)}; tests: {len(picked)}\n")
    print("\n".join(out))


if __name__ == "__main__":
    main()
