#!/bin/sh
# Emits ./cppkg_provider.cmake with the absolute path to the cpp-pkg binary
# baked in (so the generated file is machine-specific and NOT a deliverable).
#
# Usage: ./setup-provider.sh [path-to-cpp-pkg]
#        (default: `cpp-pkg` found on PATH)
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
cpp_pkg="${1:-cpp-pkg}"

"$cpp_pkg" provider-script --dir "$here"
