#!/bin/sh
# Pin json-tui v1.4.2 and prepare a cpp-pkg buildable tree in ./upstream.
#
# Usage: sh pin.sh  (from migrations/json-tui/ or any scratch dir containing
# a copy of this directory's files)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
URL=https://github.com/ArthurSonzogni/json-tui
COMMIT=717b1f9f6fe261faf4c4ee999a2d28d04b152595   # == tag v1.4.2

if [ ! -d upstream/.git ]; then
  git clone "$URL" upstream
fi
git -C upstream fetch --quiet origin "$COMMIT"
git -C upstream checkout --quiet "$COMMIT"

# --- codegen workaround (GAP: codegen-escape-hatch) -----------------------
# Upstream: configure_file(src/version.hpp.in -> ${BINARY_DIR}/src/version.hpp)
# substituting @CMAKE_PROJECT_VERSION@. cpp-pkg has no generation step, so we
# pre-generate the header into gen/src/ (listed as a private include dir).
# The version string is duplicated here and in CppPkg.toml [package].version.
mkdir -p upstream/gen/src
sed 's/@CMAKE_PROJECT_VERSION@/1.4.2/' \
  upstream/src/version.hpp.in > upstream/gen/src/version.hpp

# The checked-in manifest is the source of truth; copy it beside the sources
# (plus the resolved lockfile, so a fresh machine reuses the exact pins).
cp "$HERE/CppPkg.toml" upstream/CppPkg.toml
[ -f "$HERE/CppPkg.lock" ] && cp "$HERE/CppPkg.lock" upstream/CppPkg.lock

echo "ready: cd upstream && CPPKG_STORE=... cpp-pkg build"
