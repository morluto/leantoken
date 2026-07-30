# OpenClaw TypeScript parse diagnostic

Date: 2026-07-30

This report evaluates LeanToken's locked TypeScript parser on the exact
OpenClaw revision from issue #367. It is diagnostic evidence, not a parser
promotion result.

## Frozen inputs

- repository: `https://github.com/openclaw/openclaw.git`
- revision: `9feb6ad161877da86200693b039638dbf3411e66`
- tracked TS-family files: 23,738
- source bytes: 232,319,999
- path-and-content BLAKE3:
  `ba170cefc4bf348ea1b752d7c2fff2ea179f512854b46c5abf46b8035c80d006`
- `tree-sitter`: 0.26.11
- `tree-sitter-typescript`: 0.23.2

The checkout had the requested commit at `HEAD` and no tracked changes. The
machine-readable [aggregate](typescript-parse-diagnostic-openclaw-v1-2026-07-30.json)
contains no source text or individual paths.

## Result

The evaluator visited 61,174,856 syntax nodes. It found 810 structurally
incomplete files (3.41%), containing 1,380 `ERROR` and 40 `MISSING` nodes.
These totals exactly reproduce the issue's earlier diagnostic.

The concentration is strongly test-shaped:

| Source shape | Files | Incomplete | Incomplete rate | Recovery nodes |
| --- | ---: | ---: | ---: | ---: |
| Test | 9,183 | 722 | 7.86% | 1,208 |
| Ordinary/declaration | 14,456 | 84 | 0.58% | 203 |
| Mock/fixture/harness | 93 | 4 | 4.30% | 9 |
| Generated | 6 | 0 | 0% | 0 |

Tests account for 89.1% of incomplete files. The three largest recovery
categories account for 1,104 of 1,420 recovery nodes (77.7%); each is an
`ERROR` in test-shaped TypeScript around `formal_parameters` or a
`parenthesized_expression`. The remaining 316 nodes span 67 categories, so
the long tail is real but small.

Production extraction continues on incomplete trees. Those 810 files retain
16,494 definitions, 2,213 nested definitions, 5,798 imports, 296,786
references, 35,629 owned references, and 10,155 owner ranges. These are
observed extracted counts, not an accuracy claim: this diagnostic has no
independent semantic oracle for missing or mis-owned items.

Two release runs returned identical corpus hashes, counts, strata, and
recovery categories. The final run took 2 minutes 20 seconds on this Linux
host and reached 26,244 KiB maximum RSS. Timing is host-specific and is not a
performance gate.

## Decision

Do not fork or patch the production grammar from this evidence alone. The
current grammar is the latest locked published Rust crate, the failures are
highly concentrated in a few test-oriented recovery shapes, and partial
production extraction remains useful. LeanToken should continue to expose
`structurally_complete = false` instead of dropping the partial result.

The next parser change needs a published upstream grammar candidate or a
small, reviewable upstream fix. Rerun this exact corpus and the synthetic
fixture before promotion. A candidate must reduce recovery without regressing
complete-file extraction, incomplete-file extraction volume, determinism, or
bounds. This report does not justify path-based suppression: source-shape
labels are diagnostic strata only and never change completeness.

## Reproduction

From a clean checkout of the pinned OpenClaw revision:

```bash
cargo build --locked --release --example typescript_parse_diagnostic
target/release/examples/typescript_parse_diagnostic analyze \
  --repository /path/to/openclaw \
  --revision 9feb6ad161877da86200693b039638dbf3411e66 \
  --output /new/path/openclaw-typescript-diagnostic.json
```

The output path must not already exist. Use an outer process deadline when
running untrusted corpora; the 30-second callback applies to the diagnostic
tree parse, while the production extraction pass intentionally exercises the
existing parser API.
