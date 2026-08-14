#!/usr/bin/env bash
# Stage jeremy-rifkin/cpptrace v1.0.4 for the cpp-pkg build.
#
# Usage: ./pin.sh [dest-dir]        (default: ./upstream next to this script)
#
# After staging:
#   cd <dest-dir>
#   CPPKG_STORE=<store-dir> cpp-pkg build --config relwithdebinfo
#   ./build/demo                    # prints a stacktrace with symbols
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

# --- codegen workaround (GAPS.md: codegen-escape-hatch) -----------------
# Upstream: configure_file(cmake/in/version-hpp.in
#                          ${PROJECT_BINARY_DIR}/include/cpptrace/version.hpp)
# cpp-pkg v0 has no generation step, so substitute the @VARS@ here with the
# pinned version numbers and park the result in gen/include/, which
# CppPkg.toml exposes as a public include dir.
mkdir -p "$dest/gen/include/cpptrace"
sed -e 's/@CPPTRACE_VERSION_MAJOR@/1/' \
    -e 's/@CPPTRACE_VERSION_MINOR@/0/' \
    -e 's/@CPPTRACE_VERSION_PATCH@/4/' \
    "$dest/cmake/in/version-hpp.in" > "$dest/gen/include/cpptrace/version.hpp"

# No patches/: the source tree needs zero edits.
cp "$here/CppPkg.toml" "$dest/CppPkg.toml"
cp "$here/CppPkg.lock" "$dest/CppPkg.lock"
echo "staged cpptrace $TAG ($COMMIT) at $dest"
