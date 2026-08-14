#!/usr/bin/env python3
"""Generate CppPkg.toml targets for a subset of abseil-cpp.

Mines `absl_cc_library(...)` calls out of absl/**/CMakeLists.txt (they are
structured and greppable, per upstream convention), computes the transitive
closure of a set of root targets, and emits one [targets.<name>] block per
library with faithful per-target deps.

Workarounds encoded here (see GAPS.md):
  - cpp-pkg v0 has no interface-library target kind, so header-only abseil
    targets are emitted as static-library with a single generated stub source
    (cppkg_stub.cc). Their public deps/defines still propagate correctly.
  - cpp-pkg has no per-target compile flags, so ABSL_DEFAULT_COPTS (warning
    flags) are dropped entirely.
  - LINKOPTS/DEPS generator expressions ($<$<PLATFORM_ID:...>>) and external
    deps (Threads::Threads) cannot be expressed per-target; they are dropped
    and echoed as "# UNEXPRESSIBLE" comments in the output. The one that
    matters on macOS (CoreFoundation for absl::time) is hoisted by hand into
    the profile-wide link-flags in header.toml.

Usage: gen_toml.py <upstream-root> > targets.toml
"""
import re
import sys
from pathlib import Path

ROOTS = ["strings", "str_format", "flat_hash_map"]

# External (non-absl::) deps that need no explicit handling on macOS:
# Threads::Threads is -pthread, folded into libSystem by Apple clang.
EXTERNAL_OK = {"Threads::Threads"}

def parse_libs(root: Path):
    libs = {}
    for cml in sorted(root.glob("absl/*/CMakeLists.txt")):
        text = cml.read_text()
        subdir = cml.parent.relative_to(root).as_posix()
        # match absl_cc_library( ... ) blocks; abseil formats these with
        # balanced parens and no nested parens except generator expressions
        for m in re.finditer(r"absl_cc_library\((.*?)\n\)", text, re.S):
            body = m.group(1)
            lib = parse_block(body, subdir)
            if lib:
                libs[lib["name"]] = lib
    return libs

KEYWORDS = {"NAME", "HDRS", "SRCS", "COPTS", "DEFINES", "LINKOPTS", "DEPS",
            "PUBLIC", "TESTONLY", "DISABLE_INSTALL"}

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
    seen, order = set(), []
    def visit(name):
        if name in seen:
            return
        seen.add(name)
        lib = libs.get(name)
        if lib is None:
            raise SystemExit(f"unknown target in closure: {name}")
        for d in lib["deps"]:
            if d.startswith("absl::"):
                visit(d[len("absl::"):])
            elif d.startswith("$<"):
                pass  # generator-expression dep (e.g. CoreFoundation) — logged
            elif d in EXTERNAL_OK:
                pass  # system-provided on macOS (pthread in libSystem) — logged
            else:
                raise SystemExit(f"non-absl dep on {name}: {d}")
        order.append(name)
    for r in roots:
        visit(r)
    return order

def emit(libs, order):
    out = []
    n_iface = 0
    for name in order:
        lib = libs[name]
        out.append(f"[targets.{name}]")
        out.append('type = "static-library"')
        if lib["srcs"]:
            srcs = ", ".join(f'"{s}"' for s in lib["srcs"])
        else:
            # header-only: no interface-library kind in cpp-pkg v0 (GAP);
            # stub TU keeps the node in the graph so deps/defines propagate.
            srcs = '"cppkg_stub.cc"'
            n_iface += 1
        out.append(f"sources = [{srcs}]")
        out.append("cxx-std = 17")
        # upstream quirk: absl::strings lists itself in its own DEPS (CMake
        # tolerates self-links through the alias); cpp-pkg would see a cycle
        deps = [d[len('absl::'):] for d in lib["deps"]
                if d.startswith("absl::") and d != f"absl::{name}"]
        inc = 'public = ["."]'
        parts = [f"includes = {{ {inc} }}"]
        if lib["defines"]:
            defs = ", ".join(f'"{d}"' for d in lib["defines"])
            parts.append(f"defines = {{ public = [{defs}] }}")
        if deps:
            dl = ", ".join(f'"{d}"' for d in deps)
            parts.append(f"dependencies = {{ public = [{dl}] }}")
        out.extend(parts)
        gx = [d for d in lib["deps"] if d.startswith("$<") or d in EXTERNAL_OK]
        for g in gx + lib["linkopts"]:
            out.append(f"# UNEXPRESSIBLE (genexpr/linkopts): {g}")
        out.append("")
    sys.stderr.write(f"targets: {len(order)} ({n_iface} header-only stubs)\n")
    return "\n".join(out)

if __name__ == "__main__":
    root = Path(sys.argv[1])
    libs = parse_libs(root)
    nontest = {k: v for k, v in libs.items() if not v["testonly"]}
    sys.stderr.write(f"parsed {len(libs)} absl_cc_library calls "
                     f"({len(nontest)} non-test)\n")
    order = closure(libs, ROOTS)
    print(emit(libs, order))
