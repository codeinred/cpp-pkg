# cpp-pkg — Migration Campaign (Wave 1 → Linux)

Long-running autonomous campaign, chained stage-by-stage by Claude on
workflow-completion notifications. Committed + pushed at every stable point.
Stage statuses live in the session task list; durable decisions land in
DESIGN_CHOICES.md as always.

## Success criterion

All eight wave-1 projects (vtz, ninja, cppcheck, json-tui, googletest,
benchmark, abseil-subset, cpptrace) build and pass their parity checks via
cpp-pkg alone — **on macOS AND on Linux (keres)** — with workarounds
remaining only where a design decision explicitly deferred the feature.
Then: wave-2 readiness report (Arrow, Boost) for the user.

## Stages

- **S1 — Wave-1 migrations (macOS)** [running]: 8 migration researchers +
  backlog synthesis → migrations/*/GAPS.md, migrations/BACKLOG.md.
- **S2 — Design round**: for each top gap, competing surface designs are
  drafted, and a dedicated **tool-designer (taste) agent** — see charter —
  judges them and picks; results recorded in DESIGN_CHOICES.md, contested
  aesthetic calls additionally logged in the **Taste Memo** for user review
  (documented, reversible, non-blocking).
- **S3 — Implementation wave**: contracts-first (stub + doc-comment
  contracts, parallel implementers, integrate, adversarial review, fix) —
  the v0 build pattern. Crate tests + existing test projects stay green.
- **S4 — Re-migration**: re-run the 8 migrations against the improved tool.
  Loop S2→S4 until green (expect 2-3 loops; escape-hatch and testing-story
  gaps land first).
- **S5 — Linux bring-up (keres: ssh claude@keres, Arch x86_64, 24c/91G,
  gcc 16.1 / clang 22.1 / cmake 4.3.4 / ninja 1.13 / cargo 1.97)**:
  clone repo on keres, cargo test, fix platform assumptions (no macOS SDK,
  frameworks absent, Linux GNU driver behavior, -fPIC, gcc-16 quirks),
  test-project family green on keres. Expect toolchain-detection and
  driver work; conditional-sources support earns its keep here.
- **S6 — Corpus on Linux**: all 8 migrations green on keres (tzdata paths,
  libdwarf on ELF/DWARF — cpptrace gets EASIER on Linux, cppcheck cfg
  paths, etc.).
- **S7 — Wave-2 gate**: readiness assessment for Apache Arrow, then Boost;
  report to user with the Taste Memo for review.

## Taste charter (binding on the tool-designer agent; user's words)

Clean and *elegant*; keep things declarative; escape hatches minimal but
*sufficient*; testing, installation + exporting, conditional sources, and
dependency provisioning (zlib/zstd) solved tastefully; **familiar to users
from developed ecosystems (cargo) while providing what C++ natives need.**
Tie-breakers: (1) the declarative reading of a CppPkg.toml must never lie;
(2) simple projects stay simple — new features cost nothing when unused;
(3) prefer one orthogonal primitive over two special cases; (4) names and
shapes follow existing schema conventions (kebab-case, tables-over-flags).
When two designs survive all four, the taste agent picks and logs the
runner-up in the Taste Memo.

## Autonomy protocol

- Chain stages on completion notifications; NO user input required (user
  directive 2026-08-14): ambiguities and even expensive-to-reverse schema
  decisions are decided with best judgment under the taste charter, and
  every such decision is prominently flagged in **MORNING_REPORT.md** —
  which the user reviews in depth — with enough context to reverse it.
  Hard blockers (nothing left that can proceed) also go in the report.
- Commit + push after each stage lands. Never force-push. Migration agents
  never commit; Claude reviews and commits.
- Every loop iteration updates this file's stage log below.

## Stage log

- 2026-08-14: Campaign created. S1 running (workflow wf_890ef994-46c).
  keres access verified.
- 2026-08-14 (late): S1 COMPLETE — 8/8 green on macOS, BACKLOG.md ranked
  (12 gaps, 5 sketches). Committed + pushed. S2 design round launched.
