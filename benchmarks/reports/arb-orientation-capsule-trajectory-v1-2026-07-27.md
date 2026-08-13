# ARB orientation-capsule trajectory decision

Date: 2026-07-27

Decision: `no_measured_win`. Keep the capsule benchmark-only and do not change
production retrieval, ranking, tool descriptions, or defaults.

## Scope and provenance

The final experiment used the two already prepared local ARB repositories at
their pinned revisions. It scheduled one `prewalk` and one `prewalk_capsule`
run per task with `gpt-5.6-terra` at high reasoning for the frontier and
`gpt-5.6-luna` for the executor. Both arms used the same clean LeanToken
runtime, 4,000-token per-call retrieval bound, 30-call frontier bound, and
20-call executor bound. No larger ARB corpus was downloaded.

The redacted
[machine report](arb-orientation-capsule-trajectory-v1-2026-07-27.json)
binds raw report BLAKE3
`0b0fcd47eaff4fe7095afe06e8a694d4b4b06ab8cddfdbfac384ed9c7a7c9c44`,
classifier source
`68106f7df83e011217d8661f79ddbd41a468a9111e40f3e6bc5a4783b6659673`,
classifier binary
`ade1d435775438cb8731c831a342ce04c3c9eae3bfe0fc96e801fffbe5de88cb`,
and ten available trajectory artifacts. The raw report and private
trajectories remain local because they contain absolute paths and repository
content.

Two earlier attempts are retained locally and were not reused. V1 completed
four invalid cells before an external LeanToken `SKILL.md` read was found to be
misclassified as native task-repository retrieval. V2 was stopped after its
first invalid cell exposed two more bounded preflight shapes: exact external
skill discovery and a compound Git status/diff check. The final adapter
allowlists only exact bounded forms, charges every call, and has negative tests
for chained task-source reads and alternate shell connectors.

## Result

Both Clap patches passed the frozen offline validator:

| Clap metric | Prewalk | Capsule | Candidate delta |
| --- | ---: | ---: | ---: |
| Official success | yes | yes | no regression |
| Retrieval calls | 12 | 9 | -3 |
| Retrieval source tokens | 8,525 | 4,327 | -4,198 (-49.2%) |
| Complete capsule prompt tokens | 0 | 103 | +103 |
| Net retrieval tokens after prompt | — | — | 4,095 saved |
| Dead-end source tokens | 3,869 | 0 | -3,869 |
| Reread tokens | 4,633 | 351 | -4,282 |
| First owner evidence sequence | 5 | 3 | two calls earlier |
| Provider input tokens | 1,037,118 | 814,503 | -222,615 (-21.5%) |
| Duration | 253,163 ms | 230,468 ms | -22,695 ms (-9.0%) |

The Click pair did not produce comparable completed trajectories. The capsule
frontier created a hash-verified handoff, but its executor exceeded the frozen
20-call limit; the trace and trajectory were therefore absent. The baseline
created a complete handoff and followed the labeled owner, but its executor
used forbidden native repository retrieval at merged tool call 27. Neither run
reached authoritative validation.

The classifier fails closed on that missing candidate trajectory. It reports
one unclassified run, one unverified candidate owner route, and `null` token,
dead-end, and reread deltas for the paired decision instead of treating missing
data as zero. There were zero observed success regressions only because both
Click arms failed; that is not positive evidence.

## Interpretation

The Clap pair is strong mechanism evidence that a 46-token owner capsule can
focus a successful prewalk and save substantially more downstream retrieval
than its 103-token complete injected prompt. It is still one task and is
counterbalanced only by a Click pair that failed in different ways. The
pre-registered all-pairs gate therefore does not pass.

A future experiment may reuse the bounded contract and fail-closed classifier,
but it needs a newly frozen task set on which both control and candidate can
complete under the same executor policy. Limits or prompts must be committed
before those runs; this result must not be repaired post hoc by raising only
the candidate budget.
