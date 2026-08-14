#!/usr/bin/env python3
# [generate.tzdata-fixture]: project the fetched tzdata dir into the ${gen}
# fixture root (upstream's file(CREATE_LINK ... SYMBOLIC), as a step).
#
# Why: the test binaries derive BOTH <build>/data/tzdata and
# <build>/data/zoneinfo from one --build argument (test_main.cpp), so the
# script-fetched tzdata must appear under the same ${gen} root the
# zic-compiled zoneinfo lands in. A symlink suffices (tests only read).
#
# argv: <src-tzdata-dir> <dest> (dest arrives ${gen}-interpolated, absolute)
import os
import sys

src, dest = os.path.abspath(sys.argv[1]), sys.argv[2]
if not os.path.isdir(src):
    sys.exit("stage_tzdata: %s is not a directory (run scripts/fetch-tzdata.sh first)" % src)
os.makedirs(os.path.dirname(dest), exist_ok=True)
tmp = dest + ".tmp-link"
try:
    os.remove(tmp)
except FileNotFoundError:
    pass
os.symlink(src, tmp)
os.replace(tmp, dest)  # atomic over an existing symlink
sys.stdout.write("%s -> %s\n" % (dest, src))
