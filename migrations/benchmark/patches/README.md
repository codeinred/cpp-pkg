# patches/

Empty on purpose: the v1.9.5 migration required **zero source-tree edits**.

Everything the upstream CMake build computes at configure time (git-describe
version string, cxx_feature_check probe results, warning-flag probing) could
be expressed as hardcoded defines/flags in `CppPkg.toml` — see `GAPS.md` for
why each hardcoding is still gap data even though no patch was needed.
