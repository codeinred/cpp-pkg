// Stub translation unit for abseil's header-only targets.
//
// cpp-pkg v0 has no interface-library target kind, so every header-only
// absl_cc_library is modeled as a static-library compiling this one empty
// TU. The resulting archives are empty (ranlib warns "has no symbols") but
// the graph nodes survive, so public deps/includes/defines propagate exactly
// as upstream declared them. See GAPS.md: object-libraries/interface gap.
