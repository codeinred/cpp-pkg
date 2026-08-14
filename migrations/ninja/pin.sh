#!/bin/sh
# Pin and stage ninja v1.13.2 for the cpp-pkg migration build.
#
# Usage: ./pin.sh [DEST]   (DEST defaults to ./upstream next to this script)
#
# Steps:
#   1. clone ninja-build/ninja at the pinned release commit
#   2. copy this directory's CppPkg.toml (the source of truth) into the tree
#
# No patches/ are applied: this migration needed zero source edits. The
# browse_py.h pre-generation step that used to live here is gone: it is a
# real [generate] edge in CppPkg.toml since wave 1.
set -eu

REPO_URL="https://github.com/ninja-build/ninja"
TAG="v1.13.2"
COMMIT="3441b633c2fe2c494e958780ba0f4227b1327634"

HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="${1:-"$HERE/upstream"}"

if [ ! -d "$DEST/.git" ]; then
    git clone --branch "$TAG" --depth 1 "$REPO_URL" "$DEST"
fi
ACTUAL="$(git -C "$DEST" rev-parse HEAD)"
if [ "$ACTUAL" != "$COMMIT" ]; then
    echo "pin.sh: HEAD is $ACTUAL, expected $COMMIT" >&2
    exit 1
fi

cp "$HERE/CppPkg.toml" "$DEST/CppPkg.toml"

echo "pin.sh: staged $TAG ($COMMIT) at $DEST"
echo "build with: cd $DEST && CPPKG_STORE=... cpp-pkg build"
