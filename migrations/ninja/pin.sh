#!/bin/sh
# Pin and stage ninja v1.13.2 for the cpp-pkg migration build.
#
# Usage: ./pin.sh [DEST]   (DEST defaults to ./upstream next to this script)
#
# Steps:
#   1. clone ninja-build/ninja at the pinned release commit
#   2. pre-generate gen/build/browse_py.h — the one codegen step cpp-pkg
#      cannot express (upstream: add_custom_command running src/inline.sh);
#      the command below is the exact upstream recipe
#   3. copy this directory's CppPkg.toml (the source of truth) into the tree
#
# No patches/ are applied: this migration needed zero source edits.
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

# Codegen workaround (see GAPS.md: codegen-escape-hatch). Same command as
# upstream's add_custom_command; output goes under gen/ because browse.cc
# includes "build/browse_py.h" relative to an include dir.
mkdir -p "$DEST/gen/build"
(cd "$DEST" && sh src/inline.sh kBrowsePy < src/browse.py > gen/build/browse_py.h)

cp "$HERE/CppPkg.toml" "$DEST/CppPkg.toml"

echo "pin.sh: staged $TAG ($COMMIT) at $DEST"
echo "build with: cd $DEST && CPPKG_STORE=... cpp-pkg build"
