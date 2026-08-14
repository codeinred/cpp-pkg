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

## Executive summary — S3 (implementation) complete

Wave-1 extensions are IMPLEMENTED: cfg conditionals, [flags] + per-target
flags with the propagation fence, dev/test markers + [[run]] + `cpp-pkg
test`, two-tier [generate] + `gen`/`gen --check` (with a best-effort network
sandbox), patches (hash-spine package ids), system deps, [target-defaults],
glob negation, runtime-data staging, `cpp-pkg install`, export fixpoint.
395 tests (was 187 at v0), clippy clean, four test projects green with
warm-store cache hits and BYTE-IDENTICAL lockfiles — the no-invalidation
hash rule held. Session limit interrupted the fix pass mid-wave; resumed
from workflow cache, zero work lost (checkpoint c1e1980, final 333771c).
Release notes owed: one-time whole-project relink (link argv order);
transported-ABI re-key note. S4 re-migration launched.

## Decisions needing your review (expensive to reverse — flagged, not blocked)

From the S2 design round (full spec: `docs/design/wave1-extensions.md`,
status NORMATIVE for S3; candidates preserved under
`docs/design/candidates/`). Eight decisions, headlined by the cfg grammar:

- **cfg sub-table grammar (B3) as the one conditional spelling, with closed predicate vocabulary and quoted-key reserved combinators**
  - why now: It is the Linux enabler and will exist in user manifests at every list key from S4 onward
  - reversal cost: Effectively irreversible once wave-1 manifests and third-party adopters write it; changing spelling later means a whole-corpus rewrite and a dialect fork
  - alternative that lost: Inline when = "..." entries (rejected, not reserved — deliberately impossible to add later without a second dialect)
- **Patches fold into the package_id hash spine (<base>+patches:blake3 of len-prefixed bytes) and the lockfile gains patches rows**
  - why now: Fixes the observed per-machine fee068f7/f4632513 store split; patched sources are different sources
  - reversal cost: Store identity + lockfile ABI: reversing re-keys every patched dep's store entries and orphans committed lockfiles
  - alternative that lost: A new ConfigHashInputs field (rejected: would invalidate existing entries and misclassify source identity as configuration)
- **System deps: cppkg-sysdep-v1 hash domain entering dep_hashes (machine-local downstream keys), with lock rows recording the declaration only — never resolved machine facts**
  - why now: Kills the different-artifacts-same-key lie for system libs while keeping CppPkg.lock committable and platform-independent; makes an unused/uninstalled sysdep a need-time error, matching the green v0 cppcheck port
  - reversal cost: Hash-domain and lockfile-schema commitments; reversing to resolved-version lock rows breaks committed lockfiles on every multi-machine repo
  - alternative that lost: The winning candidate's resolved-version lock row (overruled: lockfile churns per machine, cfg.linux sysdeps unlockable from macOS, resolve-time hard errors on unrelated targets)
- **Store extraction-manifest cache keys gain an extractor-version component (Appendix A.8); ingestion-time transforms (A.1 -isystem classification, Threads rewrite, hermeticity scan) run on every manifest read**
  - why now: Without it the wave's extractor fixes reach fresh machines but silently never reach warm stores — json-tui's -Werror break would persist and cpptrace's leaked zstd entry would keep being consumed on exactly the machines that already built
  - reversal cost: A store-key schema component; removing it later re-keys all manifests again. One-time cheap manifest re-derivation on every warm store when it ships (artifacts untouched)
  - alternative that lost: Full store invalidation (violates the no-artifact-invalidation guarantee) or leaving warm stores stale (recreates the different-content-same-key lie the wave condemns)
- **exposes-targets claiming a builtin (Threads::Threads) is a hard load error with no warn-first release — the wave's only flag-day incompatibility**
  - why now: Builtin pseudo-packages (ladder step 0) cannot be shadowed; a tolerated claim to expose one is a lying manifest
  - reversal cost: Cheap to soften later (error→warning is compatible); expensive in the other direction — shipping warn-first then hardening breaks whoever ignored the warning. Three in-repo manifests break on day one, all re-edited in S4
  - alternative that lost: Warn-first deprecation release (patches-sysdeps OQ9); rejected while no out-of-repo manifest population exists
- **Interleaved link-line emission (each closure archive immediately followed by its contributor's link-flags) as the documented last-wins layering contract**
  - why now: The only position where -lrt-class raw link inputs survive GNU ld --as-needed on Arch — the corpus case is abseil base/shm_open on keres
  - reversal cost: Once documented contract, manifests will depend on exact emission positions; changing emission order later silently changes link results on ELF
  - alternative that lost: Trailing-block emission plus promoting rt/dl/m to builtin pseudo-packages (grows the builtin list without wave-1 evidence)
- **-isystem classification of all imported dep interface headers at manifest ingestion (A.1), release-noted as a real behavior change: dep headers move to the end of the include search order on every v0 manifest**
  - why now: Upstream-parity diagnostics suppression (un-breaks json-tui) — but the red team is right that it is not diagnostics-only: shadowing/same-named-header resolution can change
  - reversal cost: A silent-behavior change affecting every existing manifest at once; after adoption, reverting changes include resolution again in the other direction. Escape is per-dep (system-includes = false), not per-consumer
  - alternative that lost: Probe-time-only classification (leaves warm stores permanently inconsistent with fresh machines) or per-consumer opt-out surface (new grammar, no wave-1 evidence)
- **Exported closures with patched deps stage the patch bytes into the prefix (lib/cmake/<CmakeName>/patches/<blake3>.patch) and cite them in cppkg-manifest.json requires rows**
  - why now: The export spine's promise — consumer re-provisions the identical dependency — is unkeepable from pin+URL alone when the producer patched the dep (vtz-absl shape: unpatched tree may not even probe)
  - reversal cost: Prefix layout and consumer-contract commitment; consumers will depend on finding patch bytes by hash, so the location and requires-row shape are frozen once anything consumes an export
  - alternative that lost: Hard error on exporting any patched-dep closure (would make vtz's primary use case unexportable)

## Ambiguities resolved by judgment

- S3 fix pass upheld 3 reviewer-flagged deviations, which I ratified as
  architect (spec Amendments section): SDK-sysroot hermeticity exemption
  (hash-covered by sdk_version; load-bearing for FindZLIB SDK .tbd), lazy
  url-dep locking (sha256 already pins), and the corrected §0.3
  interpolation-position table. Two deliberate open holes tracked:
  response-file flag laundering; per-config build dirs.

- S2 red team filed 16 findings against the chosen spec; all adjudicated,
  15 produced in-place spec fixes, 1 upheld as an explicit logged decision
  (the Threads flag-day error). The two BLOCKERs the red team caught in the
  taste judge's own spec: [target-defaults] install=true detonating against
  dev/test markers (fixed via an eligibility rule), and eager [generate]
  input validation breaking fresh-clone builds (fixed: validation scoped to
  activated steps). The red-team pass earned its seat.

## Taste Memo (contested aesthetic calls; chosen + runner-up)

- **Conditional syntax (B3)** — chosen: cfg sub-tables nesting inside the scope they condition ([targets.x.cfg.linux], [flags.cfg.clang]); combinators reserved as quoted keys
  - runner-up: Inline when = "..." entries (rejected outright, not reserved)
  - why: One spelling of a conditional per language; two is how dialects fork. Near-irreversible: this grammar will exist in user files at every list key.
- **Per-target flags (B1)** — chosen: Candidate C: visibility-split grammar + propagation-class fence (unknown flags fail open; ABI/sanitizer/warning/opt classes rejected from public buckets, now matched through -Wl,/-Wa,/-Wp, pass-through wrappers)
  - runner-up: Candidate A: fully open strings, no fence
  - why: A library must not volunteer its consumers into a diagnostic or ABI policy; the fence rejects only categorically-wrong propagation. Red team closed the pass-through laundering hole (F9) — transport prefixes are unwrapped before classification.
- **Testing model (B2)** — chosen: T2: orthogonal dev + test booleans with one edge rule (non-dev may not depend on dev) + [[run]] invocation entries
  - runner-up: T1: type = "test" as a target kind
  - why: dev-graph membership and runner registration are independent facts (vtz's bench_vtz is dev-but-not-test); a type would conflate them and lie.
- **Codegen shape (B4)** — chosen: Named [generate.<name>] steps, two tiers + checked-in mode; input validation scoped to the activated step set (red-team fix F2)
  - runner-up: Target-attached template lists (killed by json-tui's multi-consumer case)
  - why: Steps are graph nodes, not target decoration. The red team showed eager input validation destroyed the laziness contract — a fresh vtz clone must build from a pure source tree, so missing inputs error only when a step activates.
- **Patches + system deps (B5+B7)** — chosen: One [dependencies] table, system = true as a third source form; pkg-config reserved; lock rows for sysdeps record the declaration only, machine facts live in the machine-local store entry (red-team fix F6, overrules the winning candidate's resolved-version lock row)
  - runner-up: Two tables, pkg-config-first resolution; resolved-version sysdep lock rows
  - why: One dependency namespace keeps the ladder simple. The candidate's lock row made CppPkg.lock machine-dependent (uncommittable) and cfg'd sysdeps unlockable cross-platform; classifying the machine probe as provisioning, not locking, reconciles sysdeps with the eager-lock contract.
- **Install/export headers (B6)** — chosen: Candidate B: headers derived from includes.public; public-headers as a rare total override — now explicitly non-cfg-conditionable (red-team fix F11)
  - runner-up: Candidate A: declared public-headers required on every exported target
  - why: The declared public interface is what ships and cannot desync. Total-override and additive-append cannot compose, so cfg conditioning of the override is a hard error pointing at conditioning includes.public instead.
- **Target-defaults vs illegal fills (red team F1)** — chosen: Eligibility rule: install/public-headers defaults skip dev/test targets (and public-headers fills only installed libraries); runtime-data fills everywhere, made safe by byte-equal dedupe
  - runner-up: Per-key opt-out syntax on targets (install = false spam, or a defaults-exclusion list)
  - why: A default must never manufacture validation errors at scale — the spec's own abseil plan died on target 1 of 281. Eligibility-by-target-shape keeps zero per-target lines; opt-out syntax would recreate the 29% repetition B9 exists to kill.
- **Compiler-specific warning batteries (red team F3)** — chosen: Per-compiler [flags.cfg.clang]/[flags.cfg.gcc] blocks in cppcheck/benchmark/abseil migration notes (abseil mirrors upstream's ABSL_LLVM_FLAGS/ABSL_GCC_FLAGS 1:1)
  - runner-up: One unconditional [flags] battery per project
  - why: -Weverything/-Wthread-safety are clang vocabulary; with -Werror in the same list an unconditional block hard-fails gcc 16 — the spec's own two-compiler acceptance gate. The grammar always had the fix; the migration notes now use it.
- **Raw -l link inputs vs --as-needed (red team F4)** — chosen: Interleaved link-line emission: each closure member's archive immediately followed by its own link-flags contribution
  - runner-up: Promote rt/dl/m to builtin pseudo-packages now (or keep trailing-block emission and document breakage)
  - why: Interleaving is the only emission position where a contributor's -lrt survives GNU ld --as-needed on Arch; promoting more builtins without evidence would grow the builtin list on speculation. §5.4's 'cfg link-flags until evidence' survives because emission now makes it correct.
- **exposes-targets = ["Threads::Threads"] flag day (red team F16)** — chosen: Hard load error whose message is the one-line fix — upheld against the red team, now an explicit logged decision in the spec
  - runner-up: Warn-first release (patches-sysdeps OQ9's alternative)
  - why: The wave's only flag-day incompatibility. All three affected manifests are in-repo and re-edited in the same S4 wave; a warned-but-tolerated claim to expose a builtin is a manifest that lies (tie-breaker 1). Warn-first protects an out-of-repo manifest population that does not exist yet.
- **Codegen sandbox degradation (red team F15)** — chosen: Warn-and-degrade: unshare -n attempted; where user namespaces are unavailable (docker/CI) the step runs unsandboxed with one warning per invocation — parity with macOS best-effort
  - runner-up: Hard abort when the namespace sandbox cannot spawn
  - why: A sandbox's failure to spawn must never be a build failure; the first containerized CI adopter would hard-fail on every tier-b step. Policy stays normative on both platforms; enforcement strength is surfaced, never silent.

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
