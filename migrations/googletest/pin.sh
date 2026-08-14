#!/bin/sh
# Pin googletest at v1.18.0 and stage the cpp-pkg manifest into the checkout.
# Usage: ./pin.sh [dest-dir]   (default dest: ./upstream)
set -eu
cd "$(dirname "$0")"
REPO=https://github.com/google/googletest
COMMIT=063de7e9578f82b369302001269680b4b1553359   # tag v1.18.0
DEST="${1:-upstream}"

rm -rf "$DEST"
git init -q "$DEST"
git -C "$DEST" remote add origin "$REPO"
git -C "$DEST" fetch -q --depth 1 origin "$COMMIT"
git -C "$DEST" checkout -q --detach FETCH_HEAD
test "$(git -C "$DEST" rev-parse HEAD)" = "$COMMIT"

# No patches/: upstream sources build unmodified.
cp CppPkg.toml "$DEST/CppPkg.toml"
echo "pinned $REPO @ $COMMIT into $DEST"
