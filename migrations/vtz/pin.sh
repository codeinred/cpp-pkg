#!/usr/bin/env bash
# Pins and stages the vtz upstream for the cpp-pkg migration build.
#
# Usage: pin.sh [dest-dir]        (default: ./upstream next to this script)
#
# - clones https://github.com/voladynamics/vtz at the pinned main commit
# - applies patches/ (none currently)
# - copies the checked-in CppPkg.toml (source of truth) into the checkout
# - runs scripts/fetch-tzdata.sh into <dest>/tzdb-runtime (the moral
#   equivalent of upstream's configure-time tzdb download/extract/zic; also
#   verifies the checked-in generated headers reproduce from tzdb 2026a)
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

# vtz source-tree patches are named vtz-*.patch (deps-*.patch files are
# dependency patches, applied separately below).
shopt -s nullglob
for p in "$HERE"/patches/vtz-*.patch; do
  git -C "$DEST" apply "$p"
  echo "applied $(basename "$p")"
done

# --- dependency patch: local abseil clone with the TESTONLY backport ---
# (cpp-pkg has no dep-patch mechanism; see patches/deps-absl-0001-*.patch)
ABSL_COMMIT=255c84dadd029fd8ad25c5efb5933e47beaa00c7   # tag 20260107.1
ABSL_DIR="$DEST/deps/absl-patched"
if [ ! -d "$ABSL_DIR/.git" ]; then
  git clone --quiet https://github.com/abseil/abseil-cpp "$ABSL_DIR"
fi
if [ "$(git -C "$ABSL_DIR" rev-parse HEAD)" != "4645a01a5cee98f8a95b83b0b7c8acd5a3ed93a1" ]; then
  git -C "$ABSL_DIR" checkout --quiet "$ABSL_COMMIT"
  python3 - "$ABSL_DIR/absl/container/CMakeLists.txt" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
old = "    absl::test_instance_tracker\n    GTest::gmock\n)"
new = "    absl::test_instance_tracker\n    GTest::gmock\n  TESTONLY\n)"
assert s.count(old) == 1, "abseil patch context not found"
open(p, 'w').write(s.replace(old, new))
EOF
  git -C "$ABSL_DIR" add -A
  GIT_AUTHOR_NAME="cpp-pkg migration" GIT_AUTHOR_EMAIL="mig@localhost" \
  GIT_AUTHOR_DATE="2026-08-14T00:00:00Z" \
  GIT_COMMITTER_NAME="cpp-pkg migration" GIT_COMMITTER_EMAIL="mig@localhost" \
  GIT_COMMITTER_DATE="2026-08-14T00:00:00Z" \
  git -C "$ABSL_DIR" commit --quiet \
    -m "backport: mark heterogeneous_lookup_testing TESTONLY (upstream master fix)"
fi
test "$(git -C "$ABSL_DIR" rev-parse HEAD)" = "4645a01a5cee98f8a95b83b0b7c8acd5a3ed93a1"

# --- dependency patch: local date clone without INTERFACE header sources ---
# (see patches/deps-date-0001-*.patch)
DATE_COMMIT=f94b8f36c6180be0021876c4a397a054fe50c6f2   # tag v3.0.4
DATE_DIR="$DEST/deps/date-patched"
if [ ! -d "$DATE_DIR/.git" ]; then
  git clone --quiet https://github.com/HowardHinnant/date "$DATE_DIR"
fi
if [ "$(git -C "$DATE_DIR" rev-parse HEAD)" != "a165a37035b2471ce969a025296e543975b5bdb1" ]; then
  git -C "$DATE_DIR" checkout --quiet "$DATE_COMMIT"
  python3 - "$DATE_DIR/CMakeLists.txt" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
start = s.index('# adding header sources just helps IDEs')
end = s.index(')\n', s.index('julian.h')) + 2
s = s[:start] + s[end:]
old = """    target_sources( date-tz
      PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_LIST_DIR}/include>$<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>/date/tz.h
      PRIVATE
        include/date/tz_private.h
        src/tz.cpp )"""
new = """    target_sources( date-tz
      PRIVATE
        include/date/tz.h
        include/date/tz_private.h
        src/tz.cpp )"""
assert s.count(old) == 1, "date patch context not found"
open(p, 'w').write(s.replace(old, new))
EOF
  git -C "$DATE_DIR" add -A
  GIT_AUTHOR_NAME="cpp-pkg migration" GIT_AUTHOR_EMAIL="mig@localhost" \
  GIT_AUTHOR_DATE="2026-08-14T00:00:00Z" \
  GIT_COMMITTER_NAME="cpp-pkg migration" GIT_COMMITTER_EMAIL="mig@localhost" \
  GIT_COMMITTER_DATE="2026-08-14T00:00:00Z" \
  git -C "$DATE_DIR" commit --quiet \
    -m "workaround: do not export headers via INTERFACE_SOURCES (cpp-pkg compiles interface sources)"
fi
test "$(git -C "$DATE_DIR" rev-parse HEAD)" = "a165a37035b2471ce969a025296e543975b5bdb1"

sed -e "s|@ABSL_PATCHED_REPO@|file://$ABSL_DIR|" \
    -e "s|@DATE_PATCHED_REPO@|file://$DATE_DIR|" \
    "$HERE/CppPkg.toml" > "$DEST/CppPkg.toml"

"$HERE/scripts/fetch-tzdata.sh" "$DEST/tzdb-runtime" "$DEST" --verify-refresh

cat <<EOF

Staged at: $DEST
Build:     cd "$DEST" && CPPKG_STORE=<store-dir> cpp-pkg build
Test:      cd "$DEST/tzdb-runtime" && ../build/test_vtz --build . --testdata ../etc/testdata
EOF
