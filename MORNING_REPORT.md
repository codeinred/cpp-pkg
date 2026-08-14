# Morning Report — cpp-pkg Migration Campaign

Maintained by Claude during the autonomous run; finalized whenever you read
it. Newest material at the top of each section. Companion documents:
CAMPAIGN.md (plan + stage log), DESIGN_CHOICES.md (every decision with
rationale), migrations/BACKLOG.md (ranked gap data, once S1 lands).

## Executive summary

**S1 complete: 8/8 wave-1 migrations green on macOS** (commit: see
`migrations/`), with honest caveats: every manifest is a macOS-only
projection, no test suite migrated *as a suite*, no port is consumable by
others yet. `migrations/BACKLOG.md` ranks 12 gaps with per-project evidence;
top five: per-target flags (the only actual build break — json-tui -Werror),
testing story, platform conditionals, codegen escape hatch, dependency
patching (contains the wave's only blocker: abseil's upstream self-edge).
Headline systemic insight (vtz): cpp-pkg's install-then-probe pipeline is
STRICTER than the FetchContent ecosystem — it found 3 real upstream
packaging bugs in 14 deps; strictness is a feature only with a patch escape
hatch. Standout parity evidence: cpp-pkg-built ninja built cpp-pkg itself;
vtz 774/774 tests incl. death tests; cppcheck byte-identical --errorlist;
benchmark bit-identical object files. S2 (design round with taste agent)
launched.

## Decisions needing your review (expensive to reverse — flagged, not blocked)

_None yet. Format when they appear: what was decided, why, what reversal
would cost, and the alternative that lost._

## Ambiguities resolved by judgment

_None yet._

## Taste Memo (contested aesthetic calls; chosen + runner-up)

_Populated by the S2 tool-designer agent rounds._

## Corpus scoreboard

| project     | macOS | linux (keres) | notes |
|-------------|-------|---------------|-------|
| vtz         | green (2 dep patches, aux tzdb script, OBJECT-lib double-compile) | — | 774/774 test parity |
| ninja       | green (posix-only projection; gtest unconditional) | — | built cpp-pkg itself |
| cppcheck    | green (matchcompiler pre-generated; runtime data hand-staged) | — | byte-identical --errorlist |
| json-tui    | green (flags gap broke build once; worked around) | — | 6/6 byte-identical checks |
| googletest  | green (both consumption modes) | — | testing-story data source |
| benchmark   | green (version stamped via patch) | — | bit-identical objects |
| abseil      | green (93-target subset via generator; self-edge patched locally) | — | 217-component probe |
| cpptrace    | green (zstd via workaround; Homebrew leak caught) | — | byte-identical traces |

## Blockers hit

_None yet._
