# patches/

Empty on purpose: the migration required **zero source-tree edits**, in both
waves.

Wave 1 expressed upstream's configure-time computation (git-describe version
string, cxx_feature_check probe results, warning-flag probing) as hardcoded,
macOS-only defines/flags. The wave-2 manifest expresses the same facts as
schema surface — `${package.version}` interpolation, `cfg` transcriptions,
`[flags]`/target flags — still with no patch. See `GAPS.md`.
