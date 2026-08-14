#!/usr/bin/env bash
# Stage jeremy-rifkin/cpptrace v1.0.4 for the cpp-pkg build.
#
# Usage: ./pin.sh [dest-dir]        (default: ./upstream next to this script)
#
# After staging:
#   cd <dest-dir>
#   CPPKG_STORE=<store-dir> cpp-pkg build --config relwithdebinfo   # library
#   cpp-pkg build demo --config relwithdebinfo && ./build/demo      # demo
#   cpp-pkg test --config relwithdebinfo                            # unit suite
set -euo pipefail

URL=https://github.com/jeremy-rifkin/cpptrace
TAG=v1.0.4
COMMIT=3db8da80111171c219ab5839905771386bee06b3

here="$(cd "$(dirname "$0")" && pwd)"
dest="${1:-$here/upstream}"

if [ ! -d "$dest/.git" ]; then
    git clone --branch "$TAG" "$URL" "$dest"
fi
git -C "$dest" checkout --detach "$COMMIT"

# Verify the pin (tags are mutable; the commit is the truth).
actual="$(git -C "$dest" rev-parse HEAD)"
if [ "$actual" != "$COMMIT" ]; then
    echo "pin mismatch: expected $COMMIT got $actual" >&2
    exit 1
fi

# No codegen here anymore: upstream's configure_file(version-hpp.in) is a
# [generate.version-header] template step in CppPkg.toml (wave 1 pre-baked
# it with sed into the source tree). No patches/: zero source-tree edits.
cp "$here/CppPkg.toml" "$dest/CppPkg.toml"
cp "$here/CppPkg.lock" "$dest/CppPkg.lock"
echo "staged cpptrace $TAG ($COMMIT) at $dest"
