#!/usr/bin/env bash
# Reproduces vtz's configure-time tzdata acquisition outside CMake.
#
# Upstream (CMakeLists.txt, top level) does at CONFIGURE time:
#   1. file(DOWNLOAD https://data.iana.org/time-zones/releases/tzdb-<V>.tar.lz)
#   2. file(ARCHIVE_EXTRACT ... DESTINATION ${BUILD}/data)
#   3. file(CREATE_LINK data/tzdb-<V> data/tzdata SYMBOLIC)
#   4. file(DOWNLOAD .../cldr/.../windowsZones.xml -> data/tzdata/) (test dep)
#   5. add_custom_command: zic -b fat -d data/zoneinfo data/tzdata/tzdata.zi
#   6. [VTZ_REFRESH_TZDATA only] regenerate include/impl/vtz/known_zones.h and
#      include/impl/vtz/embedded_tzdb_content.h from tzdata.zi (both are
#      checked into the repo, so a normal build does NOT need this step).
#
# This script performs 1-5 with pinned versions + checksums, and with
# --verify-refresh it re-derives the two generated headers and diffs them
# against the checked-in copies (proving the checked-in codegen output is
# reproducible from the pinned tzdb).
#
# Usage: fetch-tzdata.sh <dest-dir> <upstream-checkout> [--verify-refresh]
set -euo pipefail

TZDB_VERSION=2026a
TZDB_SHA256=0913509a37f26b81bb6396018ad5cdf32065374ed36e82cceb61b2ee57a94b7c
# Upstream downloads windowsZones.xml from cldr's MUTABLE main branch; we pin
# the commit that main pointed at on 2026-08-14 (hermeticity fix).
CLDR_COMMIT=eef56793a2616c8b9f2e5f62b01df2621f9a18d6
WINDOWS_ZONES_SHA256=9cf3db6a31fb382fee21b70be6feba1e82766b0fcd06e6261fb7936a73e537ff

DEST=${1:?dest dir}
SRC=${2:?upstream checkout}
VERIFY=${3:-}

mkdir -p "$DEST/data"
tarball="$DEST/tzdb-$TZDB_VERSION.tar.lz"

if [ ! -f "$tarball" ]; then
  curl -fsSL "https://data.iana.org/time-zones/releases/tzdb-$TZDB_VERSION.tar.lz" -o "$tarball"
fi
echo "$TZDB_SHA256  $tarball" | shasum -a 256 -c - >/dev/null

# cmake -E tar uses the same libarchive as file(ARCHIVE_EXTRACT) — handles .lz
( cd "$DEST/data" && cmake -E tar xf "$tarball" )
ln -sfn "tzdb-$TZDB_VERSION" "$DEST/data/tzdata"

wz="$DEST/data/tzdata/windowsZones.xml"
if [ ! -f "$wz" ]; then
  curl -fsSL "https://raw.githubusercontent.com/unicode-org/cldr/$CLDR_COMMIT/common/supplemental/windowsZones.xml" -o "$wz"
fi
echo "$WINDOWS_ZONES_SHA256  $wz" | shasum -a 256 -c - >/dev/null

zic -b fat -d "$DEST/data/zoneinfo" "$DEST/data/tzdata/tzdata.zi"

if [ "$VERIFY" = "--verify-refresh" ]; then
  zi="$DEST/data/tzdata/tzdata.zi"
  workdir=$(mktemp -d)
  # embedded_tzdb_content.h: file(READ) + string(REPLACE "\n" "\\n\"\n\"") +
  # file(WRITE "\"...\"") — i.e. each line becomes a C string literal chunk.
  python3 - "$zi" "$workdir/embedded_tzdb_content.h" <<'EOF'
import sys
text = open(sys.argv[1]).read()
out = '"' + text.replace('\n', '\\n"\n"') + '"'
open(sys.argv[2], 'w').write(out)
EOF
  # known_zones.h: configure_file of known_zones.h.in with @KNOWN_ZONES@ /
  # @KNOWN_LINKS@ from "Z <name>" / "L <target> <alias>" lines of tzdata.zi.
  python3 - "$zi" "$SRC/include/impl/vtz/known_zones.h.in" "$workdir/known_zones.h" <<'EOF'
import sys, re
zones, links = [], []
for line in open(sys.argv[1]):
    if line.startswith('Z '):
        zones.append('"%s"' % line.split()[1])
    elif line.startswith('L '):
        f = line.split()
        links.append('zone_link{ "%s", "%s" }' % (f[1], f[2]))
tpl = open(sys.argv[2]).read()
tpl = tpl.replace('@KNOWN_ZONES@', ',\n        '.join(zones))
tpl = tpl.replace('@KNOWN_LINKS@', ',\n        '.join(links))
open(sys.argv[3], 'w').write(tpl)
EOF
  diff -u "$SRC/include/impl/vtz/embedded_tzdb_content.h" "$workdir/embedded_tzdb_content.h"
  diff -u "$SRC/include/impl/vtz/known_zones.h" "$workdir/known_zones.h"
  echo "verify-refresh: checked-in generated headers match tzdb $TZDB_VERSION"
  rm -rf "$workdir"
fi

echo "tzdata ready in $DEST/data (tzdata -> tzdb-$TZDB_VERSION, zoneinfo compiled)"
