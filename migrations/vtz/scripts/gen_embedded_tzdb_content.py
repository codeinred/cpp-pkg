#!/usr/bin/env python3
# [generate.embedded-tzdb-content] (checked-in mode): tzdata.zi on stdin ->
# include/impl/vtz/embedded_tzdb_content.h on stdout.
#
# Byte-for-byte reproduction of upstream's VTZ_REFRESH_TZDATA branch:
#   file(READ tzdata.zi) + string(REPLACE "\n" "\\n\"\n\"") +
#   file(WRITE "\"${content}\"")
# i.e. every line of tzdata.zi becomes one C string-literal chunk, no
# trailing newline.
import sys

text = sys.stdin.read()
sys.stdout.write('"' + text.replace('\n', '\\n"\n"') + '"')
