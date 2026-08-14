#!/bin/sh
# Reproduce the abseil migration layout in the current directory.
#
# Usage: cd <workdir> && sh /opt/claude/cpp-pkg/migrations/abseil/pin.sh
# Then:
#   native port:  cd upstream && CPPKG_STORE=... cpp-pkg build && ./build/demo
#   consumer:     cd consumer && CPPKG_STORE=... cpp-pkg build && ./build/demo
set -eu

MIG="$(cd "$(dirname "$0")" && pwd)"
URL=https://github.com/abseil/abseil-cpp
TAG=20260526.0
COMMIT=5650e9cf76d3be4318d5fa3af38ee483ddfd5e4a

# 1. Native-port checkout: upstream sources + CppPkg.toml + demo overlay.
#    No upstream sources are modified; the overlay only ADDS files
#    (CppPkg.toml, cppkg_stub.cc, demo/main.cpp).
if [ ! -d upstream ]; then
  git clone --depth 1 --branch "$TAG" "$URL" upstream
fi
[ "$(git -C upstream rev-parse HEAD)" = "$COMMIT" ] \
  || { echo "pin mismatch: upstream HEAD != $COMMIT" >&2; exit 1; }
cp "$MIG/CppPkg.toml" upstream/
cp "$MIG/overlay/cppkg_stub.cc" upstream/
mkdir -p upstream/demo
cp "$MIG/overlay/demo/main.cpp" upstream/demo/

# 2. Patched clone for the consumer experiment: cpp-pkg cannot patch a
#    dependency, so we host a local repo with the self-dep fix committed
#    and tagged, and point the consumer manifest at it by absolute path.
rm -rf absl-patched
git clone -q upstream absl-patched
( cd absl-patched \
  && git checkout -q "$COMMIT" \
  && git apply "$MIG"/patches/0001-remove-absl-strings-self-dep.patch \
  && git -c user.email=cppkg@local -c user.name=cppkg \
       commit -aqm "remove absl::strings self-dep (cpp-pkg rejects self-edges)" \
  && git tag 20260526.0-cppkg1 )

# 3. Consumer project with the patched-repo path substituted.
mkdir -p consumer/src
sed "s|@ABSL_PATCHED_REPO@|$(pwd)/absl-patched|" \
  "$MIG/consumer/CppPkg.toml" > consumer/CppPkg.toml
cp "$MIG/consumer/src/main.cpp" consumer/src/

echo "pinned: $URL @ $TAG ($COMMIT)"
