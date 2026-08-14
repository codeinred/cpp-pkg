#!/usr/bin/env python3
# [generate.known-zones] (checked-in mode): tzdata.zi on stdin +
# known_zones.h.in template as argv[1] -> include/impl/vtz/known_zones.h on
# stdout.
#
# Byte-for-byte reproduction of upstream's VTZ_REFRESH_TZDATA branch:
# @KNOWN_ZONES@ from `Z <name> ...` lines, @KNOWN_LINKS@ as
# `zone_link{ "<target>", "<alias>" }` from `L <target> <alias>` lines,
# joined with ",\n        " (configure_file @ONLY, unix newlines).
import sys

zones, links = [], []
for line in sys.stdin:
    if line.startswith('Z '):
        zones.append('"%s"' % line.split()[1])
    elif line.startswith('L '):
        f = line.split()
        links.append('zone_link{ "%s", "%s" }' % (f[1], f[2]))

tpl = open(sys.argv[1]).read()
tpl = tpl.replace('@KNOWN_ZONES@', ',\n        '.join(zones))
tpl = tpl.replace('@KNOWN_LINKS@', ',\n        '.join(links))
sys.stdout.write(tpl)
