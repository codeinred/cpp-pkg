#!/bin/sh
# Pin cppcheck at 2.21.1 and stage the cpp-pkg manifest into the checkout.
# Usage: ./pin.sh [dest-dir]   (default dest: ./upstream)
set -eu
cd "$(dirname "$0")"
REPO=https://github.com/danmar/cppcheck
COMMIT=904cfdcf774c44b17db789c8a212e2f1c69fc833   # tag 2.21.1
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
