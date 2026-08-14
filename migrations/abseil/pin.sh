#!/bin/sh
# Reproduce the abseil migration layout in the current directory.
#
# Usage: cd <workdir> && sh /opt/claude/cpp-pkg/migrations/abseil/pin.sh
# Then:
#   native port:  cd upstream && CPPKG_STORE=... cpp-pkg build && ./build/demo
#   test suite:   cd upstream && CPPKG_STORE=... cpp-pkg test
#   install:      cd upstream && CPPKG_STORE=... cpp-pkg install --prefix <dir>
#   consumer:     cd consumer && CPPKG_STORE=... cpp-pkg build && ./build/demo
#
# Wave-2 note: the wave-1 "absl-patched" local clone (self-dep patch +
# per-machine synthetic commit + @ABSL_PATCHED_REPO@ substitution) is gone —
# self-link edges are deduped tool-side since wave 1, so the consumer
# declares plain upstream and its lockfile is committable.
set -eu

MIG="$(cd "$(dirname "$0")" && pwd)"
URL=https://github.com/abseil/abseil-cpp
TAG=20260526.0
COMMIT=5650e9cf76d3be4318d5fa3af38ee483ddfd5e4a

# Native-port checkout: upstream sources + CppPkg.toml + demo overlay.
# No upstream sources are modified; the overlay only ADDS files
# (CppPkg.toml, cppkg_stub.cc, demo/main.cpp).
if [ ! -d upstream ]; then
  git clone --depth 1 --branch "$TAG" "$URL" upstream
fi
[ "$(git -C upstream rev-parse HEAD)" = "$COMMIT" ] \
  || { echo "pin mismatch: upstream HEAD != $COMMIT" >&2; exit 1; }
cp "$MIG/CppPkg.toml" upstream/
cp "$MIG/overlay/cppkg_stub.cc" upstream/
mkdir -p upstream/demo
cp "$MIG/overlay/demo/main.cpp" upstream/demo/

# Consumer project: abseil as an ordinary (unpatched) dependency.
mkdir -p consumer/src
cp "$MIG/consumer/CppPkg.toml" consumer/
cp "$MIG/consumer/src/main.cpp" consumer/src/

echo "pinned: $URL @ $TAG ($COMMIT)"
