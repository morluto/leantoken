# ARB trace2code smoke baseline v1

This report freezes a small Agent Retrieval Bench (ARB) trace2code baseline for
experiment design. It evaluates two public tasks and does not change production
retrieval.

## Reproducibility

- Dataset: `eyuansu71/agent_retrieval_bench`
- Dataset revision:
  `c50401f20c60a8c45da94f2ef785ac9a99a6eb55`
- Upstream adapter revision:
  `d04953371d962ec314fb15d642255ed4e9dadd40`
- Release: `v2_trace2code`
- Compressed release size: 39,295,446 bytes
- Release SHA-256:
  `19b252e8cfff42107fedc74005dbb6972f2970af33651ce0c1571546819e41c4`
- Extracted `samples.jsonl` BLAKE3:
  `331bb5f0f8b4660eb9329494701439df98cf05ec0782b3812cb8126e3470eb71`
- Generated manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`
- Tokenizer: `cl100k_base`, exact counts

The smoke set deterministically selects one Rust task from `clap-rs/clap` and
one Python task from `pallets/click`. Their target repositories are checked out
at the exact ARB base revisions. The generated manifest contains two tasks,
three root-cause files, and three root-cause line anchors.

The machine-readable result is
[`arb-trace2code-smoke-baseline-v1-2026-07-27.json`](arb-trace2code-smoke-baseline-v1-2026-07-27.json).

## Results

| Task | Gold files selected | Gold files generated | Anchors selected | Returned source |
| --- | ---: | ---: | ---: | ---: |
| Clap / Rust | 0/2 | 0/2 | 0/2 | 797 tokens |
| Click / Python | 0/1 | 1/1 | 0/1 | 354 tokens |
| Aggregate | 0/3 | 1/3 | 0/3 | 1,151 tokens |

The first response used 2,367 complete JSON tokens versus 92,767 tokens for the
scripted discovery-plus-full-file envelope, a 97.4% reduction. Returned source
used 1,151 tokens versus an 80,214-token full-file oracle, a 98.6% reduction.
Those reductions are not useful-task savings here because none of the three
gold files survived selection.

The two-turn path resent no known fragments. It used 7,563 complete JSON tokens
and exposed known-hash suppression through the compact omission summary.

On this host, Clap indexed 588 files and 1,696 chunks in 1.70 seconds; Click
indexed 138 files and 428 chunks in 0.43 seconds. Task-level warm-context
medians were 73.8 ms and 130.9 ms respectively. Timings depend on host and
filesystem cache state.

## Interpretation

This baseline exposes two distinct failure owners:

- The Clap root-cause files never entered the generated candidate set. A
  ranking-only change cannot recover them.
- The Click root-cause file entered the candidate set but was not selected.
  Its trace contains broad words such as `type` and `default`, which produced
  many plausible regions in the correct file but did not retain that file in
  the bounded response.

The run also caught a harness compatibility defect. Compact production
responses aggregate known-hash suppression in `omission_summary`, while the
benchmark required a verbose per-candidate omission. The harness now accepts
either representation and retains compact production-equivalent responses.

## Decision

Adopt the pinned ARB trace2code adapter and this smoke baseline as diagnostic
infrastructure. Do not promote a production ranking change from two tasks.

Use the failures to evaluate an explicit workflow-evidence contract before
trying broader ranking changes. The experiment must distinguish failure-trace,
symbol, path, and test-intent evidence, run as a frozen A/B, preserve exact
token budgets, and report candidate generation separately from selection.

This is not a complete ARB run. The public release contains 101 trace2code
samples across seven repositories; this report covers two samples and two
repositories. It does not measure end-to-end patch success, agent trajectories,
or scalability.
