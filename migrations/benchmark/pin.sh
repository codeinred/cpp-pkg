#!/bin/sh
# Pin google/benchmark at v1.9.5 and stage the cpp-pkg migration manifest.
#
# Layout produced:
#   ./upstream/            clone at the pinned commit, CppPkg.toml copied in
#
# Reproduce the source-mode build:
#   cd upstream && CPPKG_STORE=... cpp-pkg build && ./build/basic-bench
# Reproduce the dependency-mode build:
#   cd ../consumer && CPPKG_STORE=... cpp-pkg build && ./build/bench-demo
set -eu

URL=https://github.com/google/benchmark
TAG=v1.9.5
COMMIT=192ef10025eb2c4cdd392bc502f0c852196baa48
HERE=$(cd "$(dirname "$0")" && pwd)

if [ ! -d "$HERE/upstream/.git" ]; then
  git clone "$URL" "$HERE/upstream"
fi
git -C "$HERE/upstream" fetch --tags origin "$COMMIT"
git -C "$HERE/upstream" checkout --detach "$COMMIT"
test "$(git -C "$HERE/upstream" rev-parse HEAD)" = "$COMMIT"
test "$(git -C "$HERE/upstream" describe --tags)" = "$TAG"

# No patches/ to apply: the migration needed zero source-tree edits.

cp "$HERE/CppPkg.toml" "$HERE/upstream/CppPkg.toml"
echo "pinned $URL @ $TAG ($COMMIT); manifest staged at upstream/CppPkg.toml"
