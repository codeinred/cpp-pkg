export const meta = {
  name: 'cppkg-s4-remigration',
  description: 'Re-migrate the 8-project corpus against the extended tool; measure dissolved vs remaining gaps',
  phases: [
    { title: 'Remigrate', detail: '8 agents rewrite their migrations with the new schema features' },
    { title: 'Verdict', detail: 'synthesis: dissolved/remaining scorecard + loop-or-Linux recommendation' },
  ],
}

const REPO = '/opt/claude/cpp-pkg'
const BIN = REPO + '/target/debug/cpp-pkg'
const SCRATCH = '/private/tmp/claude-501/-opt-claude-cpp-pkg/b18b77fb-4da8-4c8c-bd66-fceb8911b272/scratchpad'

const RESULT_SCHEMA = {
  type: 'object',
  required: ['status', 'dissolved', 'remaining', 'report'],
  properties: {
    status: { type: 'string', enum: ['green', 'partial', 'blocked'] },
    dissolved: { type: 'array', items: { type: 'string' }, description: 'wave-1 workarounds now expressed in real schema syntax (name each, cite the feature that dissolved it)' },
    remaining: { type: 'array', items: { type: 'object', required: ['title', 'severity', 'description'], properties: { title: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, description: { type: 'string' } } }, description: 'workarounds still required, NEW bugs in the wave-1 features, or gaps the wave did not cover' },
    report: { type: 'string' },
  },
}

const COMMON = `You are a RE-MIGRATION researcher for cpp-pkg (repo ${REPO}, git — do NOT commit). The tool just gained the wave-1 extensions: read ${REPO}/CPPKG_TOML.md (updated normative user schema) and skim ${REPO}/docs/design/wave1-extensions.md for your project's migration note (each wave-1 port has one). Binary: ${BIN} (do NOT rebuild or modify src/). Your project's existing port is ${REPO}/migrations/<key>/ with its GAPS.md from wave 1.

MISSION: rewrite the migration to use the new features NATIVELY — every workaround the wave was designed to dissolve should now be real syntax: cfg sub-tables instead of macOS-only projections (write the Linux branches NOW from upstream's own build logic — S5 validates them on a real Linux box next stage, so wrong guesses are fine but absent branches are not); [flags]/target flags instead of dropped or profile-smuggled flags; [generate] steps instead of pre-generated patches/ files (delete those files where a step replaces them); patches = [...] instead of file:// cloned deps; dev/test markers + [[run]] entries for the ACTUAL test suite (vtz: all 774; ninja: the gtest binary; etc.) run via cpp-pkg test; [target-defaults], glob !-negation, runtime-data, system deps where applicable.

Method: fresh store (CPPKG_STORE=${SCRATCH}/store-s4-<key>), build + run parity checks from wave 1 again, run 'cpp-pkg test' where the project has tests, verify second-build cache hit. Update migrations/<key>/: the manifest files, README (new invocations), and REWRITE GAPS.md as the wave-2 edition: a 'Dissolved' section (workaround -> feature), and 'Remaining' (honest — including NEW bugs you find in the wave-1 features themselves; those are the most valuable findings now). Do not water anything down; blocked-with-diagnosis beats fake green. Structured output per the schema.

Machine: macOS arm64, Apple clang 21, cmake 4.4, network OK.`

const KEYS = ['vtz', 'ninja', 'cppcheck', 'json-tui', 'googletest', 'benchmark', 'abseil', 'cpptrace']

phase('Remigrate')
const results = await parallel(KEYS.map(k => () =>
  agent(COMMON + `\n\nYOUR PROJECT: '${k}' (${REPO}/migrations/${k}/).` + (k === 'abseil' ? ' Also regenerate via your generator with [target-defaults] + [flags] and report the new TOML line count vs the old 660 — the ergonomics number the backlog asked for.' : '') + (k === 'vtz' ? ' The zic pipeline is the acid test for [generate] tier-b; the tzdata fetch remains an aux script this wave (pinned-asset fetch is deferred by design) — but known_zones.h and the embed step should be real generate steps now.' : ''), { label: 's4:' + k, phase: 'Remigrate', schema: RESULT_SCHEMA })
    .then(r => ({ key: k, r }))))

phase('Verdict')
const usable = results.filter(Boolean)
const dump = usable.map(x => '## ' + x.key + ' — ' + x.r.status + '\nDissolved: ' + x.r.dissolved.join('; ') + '\nRemaining: ' + x.r.remaining.map(g => '[' + g.severity + '] ' + g.title + ': ' + g.description).join(' | ') + '\nReport: ' + x.r.report).join('\n\n')

const verdict = await agent(`You are the S4 SYNTHESIS agent for cpp-pkg (repo ${REPO}; do NOT commit). Eight re-migration results below (full GAPS.md wave-2 editions under migrations/*/). Update ${REPO}/migrations/BACKLOG.md: mark dissolved items DONE with the dissolving feature named, add any NEW bugs/gaps found in the wave-1 features (ranked), and write a final VERDICT section answering: (a) is another S2-S3 design/implement loop required before Linux, or are remaining items minor enough to fold into the Linux stage? (b) the concrete recommended next-loop scope if one is needed. Criteria: blockers/majors in the wave-1 features themselves force a loop; deferred-by-design items (pinned-asset fetch, per-source transforms) do NOT. Final message: scoreboard line per project + your verdict + top 5 remaining items.\n\n` + dump, { label: 's4-verdict', phase: 'Verdict' })

return { statuses: usable.map(x => x.key + ': ' + x.r.status), verdict }