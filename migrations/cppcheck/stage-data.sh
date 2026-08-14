#!/bin/sh
# Stage cppcheck's runtime data next to the built executable, mirroring the
# upstream CMake post-build targets copy_cfg / copy_addons / copy_platforms /
# remove_unsigned_platforms. cppcheck resolves cfg/*.cfg, platforms/*.xml and
# addons/* relative to the executable path (or the compiled-in FILESDIR,
# which defaults to /usr/local/share/Cppcheck and is normally absent).
#
# Usage: ./stage-data.sh [upstream-dir]   (default: ./upstream)
set -eu
cd "$(dirname "$0")"
SRC="${1:-upstream}"
BIN_DIR="$SRC/build"
test -x "$BIN_DIR/cppcheck" || { echo "no $BIN_DIR/cppcheck - build first" >&2; exit 1; }

rm -rf "$BIN_DIR/cfg" "$BIN_DIR/platforms" "$BIN_DIR/addons"
cp -R "$SRC/cfg"       "$BIN_DIR/cfg"
cp -R "$SRC/platforms" "$BIN_DIR/platforms"
cp -R "$SRC/addons"    "$BIN_DIR/addons"
# upstream deletes these two from the staged copy (remove_unsigned_platforms)
rm -f "$BIN_DIR/platforms/unix32-unsigned.xml" "$BIN_DIR/platforms/unix64-unsigned.xml"
echo "staged cfg/ platforms/ addons/ into $BIN_DIR"
