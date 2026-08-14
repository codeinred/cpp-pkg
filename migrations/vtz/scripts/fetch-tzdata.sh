#!/usr/bin/env bash
# Fetches the pinned tzdb release + windowsZones.xml (vtz's configure-time
# file(DOWNLOAD)/file(ARCHIVE_EXTRACT)/file(CREATE_LINK), hermeticized).
#
# Wave 2: this script is ONLY the network fetch — [generate] tier d
# (pinned-asset fetch) is deferred by design. Everything downstream of the
# fetched bytes is now a real [generate] step in CppPkg.toml:
#   - zic compile of data/zoneinfo        -> [generate.zoneinfo]
#   - tzdata symlink into the ${gen} root -> [generate.tzdata-fixture]
#   - embedded_tzdb_content.h refresh     -> [generate.embedded-tzdb-content]
#     known_zones.h refresh                  [generate.known-zones]
#     (checked-in mode; `cpp-pkg gen --check` replaces the wave-1
#     --verify-refresh re-implementation)
#
# Usage: fetch-tzdata.sh <dest-dir>       (dest = <checkout>/tzdb-runtime)
set -euo pipefail

TZDB_VERSION=2026a
TZDB_SHA256=0913509a37f26b81bb6396018ad5cdf32065374ed36e82cceb61b2ee57a94b7c
# Upstream downloads windowsZones.xml from cldr's MUTABLE main branch; we pin
# the commit that main pointed at on 2026-08-14 (hermeticity fix).
CLDR_COMMIT=eef56793a2616c8b9f2e5f62b01df2621f9a18d6
WINDOWS_ZONES_SHA256=9cf3db6a31fb382fee21b70be6feba1e82766b0fcd06e6261fb7936a73e537ff

DEST=${1:?dest dir}

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

echo "tzdata ready in $DEST/data (tzdata -> tzdb-$TZDB_VERSION)"
