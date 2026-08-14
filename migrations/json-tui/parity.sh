#!/bin/sh
# Parity check: upstream CMake build vs cpp-pkg build of json-tui.
# Usage: sh parity.sh <cmake-binary-dir> <cppkg-binary-dir>
# (each dir must contain json-tui and tests executables)
set -eu
CMAKE_BIN="$1"
CPPKG_BIN="$2"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

printf '{"name":"json-tui","versions":[1,2,3],"nested":{"ok":true,"pi":3.14}}' \
  > "$TMP/sample.json"

fail=0

for t in version help keybinding; do
  case $t in
    version)    args="--version" ;;
    help)       args="--help" ;;
    keybinding) args="--keybinding" ;;
  esac
  "$CMAKE_BIN/json-tui" $args > "$TMP/$t.a.out" 2>&1 || true
  "$CPPKG_BIN/json-tui" $args > "$TMP/$t.b.out" 2>&1 || true
  cmp -s "$TMP/$t.a.out" "$TMP/$t.b.out" && echo "PASS $t" || { echo "FAIL $t"; fail=1; }
done

# invalid JSON on stdin: parse error + exit code
ra=0; printf '{"bad":' | "$CMAKE_BIN/json-tui"  > "$TMP/bad.a.out" 2>&1 || ra=$?
rb=0; printf '{"bad":' | "$CPPKG_BIN/json-tui"  > "$TMP/bad.b.out" 2>&1 || rb=$?
{ cmp -s "$TMP/bad.a.out" "$TMP/bad.b.out" && [ "$ra" = "$rb" ]; } \
  && echo "PASS bad-json (exit $ra)" || { echo "FAIL bad-json"; fail=1; }

# valid JSON, non-tty: first rendered UI frames (timeout kills the UI loop)
cat "$TMP/sample.json" | timeout 5 "$CMAKE_BIN/json-tui" > "$TMP/ui.a.out" 2>&1 || true
cat "$TMP/sample.json" | timeout 5 "$CPPKG_BIN/json-tui" > "$TMP/ui.b.out" 2>&1 || true
cmp -s "$TMP/ui.a.out" "$TMP/ui.b.out" \
  && echo "PASS ui-render ($(wc -c < "$TMP/ui.a.out" | tr -d ' ') bytes)" \
  || { echo "FAIL ui-render"; fail=1; }

# gtest: same test list, both green (paths in gtest_main banner differ; compare
# from the first [==========] line). gtest timing lines ("(0 ms total)") are
# stable at 0 ms for these tests; if flaky, strip them too.
"$CMAKE_BIN/tests" > "$TMP/tests.a.out" 2>&1 || { echo "FAIL tests (cmake side red)"; fail=1; }
"$CPPKG_BIN/tests" > "$TMP/tests.b.out" 2>&1 || { echo "FAIL tests (cppkg side red)"; fail=1; }
for side in a b; do
  sed -n '/^\[==========\]/,$p' "$TMP/tests.$side.out" > "$TMP/tests.$side.trim"
done
cmp -s "$TMP/tests.a.trim" "$TMP/tests.b.trim" \
  && echo "PASS tests ($(grep -c '^\[       OK \]' "$TMP/tests.a.out") cases)" \
  || { echo "FAIL tests"; fail=1; }

exit $fail
