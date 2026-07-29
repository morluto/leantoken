# Bounded regex literal-extractor trial

Date: 2026-07-29

Decision: retain the bounded `regex-syntax` prefix/suffix fallback after the
existing mandatory-literal planner. The trial preserved forced-full-scan result
parity and removed thousands of chunk verifications for the finite-repetition
shapes measured here.

## Evidence identity

- LeanToken base revision:
  `1800f7020da9be38a9bebb56114a54e3e3dfb21e`, with the issue #365/#368/#369
  working-tree changes under evaluation.
- Release profiling binary SHA-256:
  `9eae399359d056964ed8db5e6a4d32a496a638bb12a99190f07046158ddb4105`.
- Harness: `examples/real_repository_profile.rs`, three warmed optimized samples
  per shape. Timings are descriptive only.
- Toolchain: `rustc 1.95.0 (59807616e 2026-04-14)`.
- Host: Linux `6.8.0-136-generic`, x86-64, four available processors.
- Hermes Agent: dirty working tree based on
  `9ce704c7236d6fa1765108757f03637d62eaac6b`, existing 6,160-file generation.
- OpenClaw: clean `9feb6ad161877da86200693b039638dbf3411e66`;
  a disposable `**/*.md` scoped index contained 1,159 files. The existing full
  28,977-file index was retained separately.

The two fixed benchmark shapes exercise a finite repetition whose repeated
unit is individually shorter than one trigram. One shape has a Hermes match;
the other is absent. Raw regexes, extracted literals, paths, and repository
identifiers are not emitted by the planner diagnostics.

## Results

| Corpus and shape | Result parity | Planned candidates verified | Full-scan chunks verified | Retained hits | Optimized p50 |
| --- | --- | ---: | ---: | ---: | ---: |
| Hermes finite-repeat positive | yes | 7 | 6,222 | 1 | 28.1 ms |
| Hermes finite-repeat negative | yes | 0 | 6,222 | 0 | 26.0 ms |
| OpenClaw Markdown finite-repeat first shape | yes | 0 | 3,643 | 0 | 32.5 ms |
| OpenClaw Markdown finite-repeat negative | yes | 0 | 3,643 | 0 | 28.0 ms |

Every optimized arm reported `trigram`, `prefix_literals`, two HIR nodes, one
four-byte term, and no fallback reason. Every oracle arm reported `full_scan`
and `planning_disabled`. The complete OpenClaw index cannot execute the oracle:
it returns the intended `regex_full_scan_files` reason with observed 28,977 and
limit 10,000. The same optimized shapes complete against that full index with
zero candidate chunks, showing why a sound plan affects answerability there,
but those runs are not counted as parity evidence.

## Interpretation and limits

The result supports this narrow extension, not arbitrary HIR expansion.
`regex-syntax::hir::literal::Extractor` remains capped at 16 alternatives and
the surrounding planner remains capped at 256 HIR nodes, 32 terms, and 256
aggregate term bytes. Every alternative must yield a case-sensitive ASCII word
term of at least three bytes, and the original regex verifies every candidate.
Case-insensitive Unicode and infinite or non-indexable sequences still fall
back.

The trial covers two finite-repetition shapes on two repositories; it is not a
production regex-distribution study. Optimized wall time includes service and
response overhead, while the oracle was sampled for correctness and work
counters rather than latency. Future extensions still require their own
differential corpus and downstream-work evidence.
