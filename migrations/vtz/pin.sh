#!/usr/bin/env bash
# Pins and stages the vtz upstream for the cpp-pkg migration build.
#
# Usage: pin.sh [dest-dir]        (default: ./upstream next to this script)
#
# - clones https://github.com/voladynamics/vtz at the pinned main commit
# - stages the checked-in CppPkg.toml (source of truth), the [generate]
#   helper scripts (-> etc/cppkg/), and the absl dependency patch
#   (-> patches/) into the checkout
# - runs scripts/fetch-tzdata.sh into <dest>/tzdb-runtime (pinned tzdb
#   download; [generate] tier d — asset fetch — is deferred by design)
#
# Wave 2: the wave-1 local-clone machinery for patched absl/date is GONE —
# absl is patched via the manifest's `patches = [...]`, and date's
# INTERFACE_SOURCES headers no longer trip the extractor. The staged
# CppPkg.toml is byte-identical to the checked-in one (no placeholders).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VTZ_URL=https://github.com/voladynamics/vtz
VTZ_COMMIT=8d6ea8f35ed18fb72b9796a1d9a843df0529baf0   # main @ 2026-08-13 (no release tag pinned; brief says main)

DEST=${1:-"$HERE/upstream"}

if [ ! -d "$DEST/.git" ]; then
  git clone "$VTZ_URL" "$DEST"
fi
git -C "$DEST" fetch origin "$VTZ_COMMIT" 2>/dev/null || true
git -C "$DEST" checkout --quiet "$VTZ_COMMIT"
test "$(git -C "$DEST" rev-parse HEAD)" = "$VTZ_COMMIT"

# vtz source-tree patches are named vtz-*.patch (none currently).
shopt -s nullglob
for p in "$HERE"/patches/vtz-*.patch; do
  git -C "$DEST" apply "$p"
  echo "applied $(basename "$p")"
done

cp "$HERE/CppPkg.toml" "$DEST/CppPkg.toml"

mkdir -p "$DEST/etc/cppkg" "$DEST/patches"
cp "$HERE"/scripts/gen_embedded_tzdb_content.py \
   "$HERE"/scripts/gen_known_zones.py \
   "$HERE"/scripts/stage_tzdata.py \
   "$DEST/etc/cppkg/"
cp "$HERE"/patches/absl-0001-mark-heterogeneous_lookup_testing-TESTONLY.patch \
   "$DEST/patches/"

"$HERE/scripts/fetch-tzdata.sh" "$DEST/tzdb-runtime"

cat <<EOF

Staged at: $DEST
Build:     cd "$DEST" && CPPKG_STORE=<store-dir> cpp-pkg build
Test:      cd "$DEST" && CPPKG_STORE=<store-dir> cpp-pkg test
Codegen:   cd "$DEST" && CPPKG_STORE=<store-dir> cpp-pkg gen --check
EOF
