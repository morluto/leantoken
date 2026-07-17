# Product measurement

LeanToken separates retrieval quality, protocol cost, and model task success.
No single fixture is allowed to stand in for all three.

## Frozen retrieval sets

`benchmarks/representative.json` is the visible development set. Its eight
tasks were constructed retrospectively from public fixes, so it is useful for
debugging but not for generalization claims.

`benchmarks/validation.json` contains prompts and line labels collected from
issues that were open on 2026-07-15 and from pinned source. It records issue,
prompt, and label provenance and contains no future fix commit. The set was
used during ranking and range tuning, so report it as prospective validation
data, not as a blind holdout. Treat the manifest BLAKE3 hash as the dataset
identity.

Prepare each repository at the manifest's exact `base_revision`, then run:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/validation.json \
  --repos-root target/validation-repos \
  --output target/validation.json

cargo run --release --example benchmark_ablation -- \
  --baseline target/validation-baseline.json \
  --candidate target/validation.json
```

The runner reports file and line-anchor recall, returned source, complete JSON,
unlabeled fragment cost, repeated ranges, known-hash resends, and two-turn cost.
It also reports sorted pre-selection candidate paths and signal summaries for
labeled files. Candidate recall separates query/index generation failures from
deduplication, ranking, and allocation failures without adding diagnostics to
the MCP response schema.
Do not alter prompts, labels, budgets, or pinned revisions after inspecting a
candidate. Freeze a new dataset version instead.

The prospective-validation report for candidate `2c0388d` and its identical-
manifest ablation are archived in `benchmarks/reports/`. Against runtime
revision `0b6f80b`, the candidate improved file recall from 7/11 to 8/11 and
line-anchor recall from 13/38 to 17/38. It returned four Express anchors where
the baseline returned none. Dead-end source fell from 1,139 to 1,081 tokens;
complete first-response JSON increased from 5,569 to 5,797 tokens and complete
two-turn JSON from 12,367 to 12,410 tokens. Both reports use manifest BLAKE3
`5991d8a643a873ef61d5a4122f52abd7f589a5403d13ab609ebc6b9428e73d9a`.
The four validation prompts lead with their task locus. They do not establish
how original-order prose selection behaves when a long symptom narrative comes
before decisive terms in a later sentence; retain that phrasing class in future
validation rather than treating this ablation as a general language result.

The next frozen validation ablation used those candidate diagnostics. All 11
labeled files appeared before selection, while only 8 were returned, locating
the remaining misses after candidate generation. Inspection found JavaScript
assignment symbols whose signatures included function bodies because the
captured assignment node had no direct `body` field. That allowed an
`app.render` call inside `res.render` to look like owner evidence. Truncating a
signature at the first descendant body recovered `lib/application.js`. By
itself, that correction increased returned-file recall from 8/11 to 9/11 and
line-anchor recall from 17/38 to 21/38. Dead-end source fell from 1,081 to 941
tokens; complete first-response JSON increased from 5,797 to 6,040 tokens and
complete two-turn JSON from 12,410 to 12,486 tokens.

A separate path ablation rewarded adjacent trailing words from qualified
tokens. It raised validation file recall to 10/11, but an explicitly diagnostic
rerun of the consumed holdout fell from 9/25 to 7/25 returned files and from
10/111 to 8/111 line anchors. Candidate recall remained 19/25, locating the
regression in selection. The path rule was removed rather than tuned against
individual consumed tasks. A generic second-path allocation candidate was also
rejected: it reduced validation line recall and added 358 dead-end source
tokens despite finding one more labeled file. Direct qualified symbol lookup
was rejected for context assembly because a large Flask declaration displaced
more useful labeled ranges. None of these consumed-holdout diagnostics are
generalization evidence.

## Sealed holdout lifecycle

`benchmarks/holdout.json` is the unseen set for the runtime tree at its frozen
`candidate_revision`. It contains nine tasks collected from issues that were
open on 2026-07-16, spanning Rust, Python, JavaScript, TypeScript, Go, Ruby, and
five task shapes. Collection used issue reports and source at exact HEAD
revisions. It did not use pull requests, patches, fix commits, or proposed
branches. Tasks with an ambiguous policy decision or an external source owner
were rejected before sealing.

The manifest freezes prompts, queries, labels, line anchors, budgets, source
revisions, evaluation procedure, and reclassification rule. For a blind run,
the benchmark refuses uncommitted runtime changes and verifies that `src/`,
`Cargo.toml`, and `Cargo.lock` match the candidate revision. The report also
records the harness revision, dirty state, manifest BLAKE3 identity, and whether
the runtime-tree check passed.

Prepare clean checkouts in a new directory using each manifest URL and revision,
then run the report once:

```bash
mkdir -p target/holdout-repos
jq -r '.corpora[] | [.directory, .url, .base_revision] | @tsv' \
  benchmarks/holdout.json |
while IFS=$'\t' read -r directory url revision; do
  if test -e "target/holdout-repos/$directory"; then
    printf 'refusing existing path: %s\n' "target/holdout-repos/$directory" >&2
    exit 1
  fi
  git init "target/holdout-repos/$directory"
  git -C "target/holdout-repos/$directory" remote add origin "$url"
  git -C "target/holdout-repos/$directory" fetch --depth=1 origin "$revision"
  git -C "target/holdout-repos/$directory" checkout --detach "$revision"
done

cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/holdout.json \
  --repos-root target/holdout-repos \
  --preflight-only

cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/holdout.json \
  --repos-root target/holdout-repos \
  --output target/holdout-report.json
```

Archive the unedited report before inspecting it. Inspection consumes the set
for that candidate: do not tune against the result and present another candidate
as blind on the same tasks. If the tasks become tuning inputs, copy them to a
prospective validation manifest and collect a new unseen holdout with a new hash.

After a holdout is consumed, `--consumed-diagnostic` permits an explicitly
non-blind rerun against a changed runtime tree. The report preserves the
manifest hash, sets `diagnostic_only` to true, leaves candidate-tree verification
unset, and states that the result is not generalization evidence. Use this only
for regression diagnosis; it does not refresh or replace the holdout.

The holdout was evaluated once on 2026-07-16 and is now consumed for candidate
`0b6f80bb4e9d356443ebd130be1d04c0254111cb`. The unchanged Linux x86-64 report
is [`../benchmarks/reports/holdout-linux-x86_64-2026-07-16.json`](../benchmarks/reports/holdout-linux-x86_64-2026-07-16.json),
with manifest hash
`a61d9672ca483dbebbbe75d5b947cbfdcbc56a41218b2f756771667b8912e263`.
The clean harness verified the candidate runtime tree before evaluation.

This was a negative retrieval result: aggregate labeled-file recall was 36%,
line-anchor recall was 9%, and 2,806 source tokens were returned in unlabeled
fragments. Express reached 100% file recall, Gin 75%, Requests, Lodash, and Rack
50%, while Serde, Flask, Vue, and TypeORM returned none of their labeled files.
Known-hash resends remained zero and the estimated repeated-range cost was 62
source tokens. The large apparent source savings are not a product win because
the responses frequently omitted the labeled evidence. These results were not
used to alter ranking, prompts, labels, queries, ranges, or budgets.

## Model-in-the-loop A/B

`model_ab` executes the same frozen task across four arms:

- ordinary filesystem tools;
- progressive LeanToken retrieval;
- one-shot `leantoken_context`;
- frontier prewalk followed by a cheaper executor.

Each run receives a fresh detached Git worktree at the same revision. The
external adapter receives one JSON request on stdin and must return one JSON
result on stdout. Start from `benchmarks/model_ab.example.json`:

```bash
cargo run --release --example model_ab -- \
  --manifest target/model_ab.json \
  --adapter /path/to/provider-adapter \
  --repetitions 5 \
  --output target/model_ab-report.json
```

The adapter result contract is provider-neutral:

```json
{
  "task_success": false,
  "total_input_tokens": 12345,
  "total_output_tokens": 678,
  "provider_reported_cost_usd": 0.42,
  "tool_calls": 12,
  "rereads": 2,
  "reread_tokens": 300,
  "failed_searches": 1,
  "dead_end_reads": 2,
  "provider_usage": {},
  "evidence_receipt": null,
  "repository_generation": 42
}
```

`task_success` is the agent's own claim; the authoritative report value is
replaced with the frozen success-command result.

The adapter owns provider authentication, the actual model/tool harness, and
raw trace retention. The harness runs the frozen success command itself after
each arm; agent-reported success is retained only as a diagnostic. The adapter
reports provider input and output tokens, cost, tool calls, rereads, reread
tokens, failed searches, dead-end reads, raw receipts, and the observed
repository generation. For prewalk, it must transfer the complete
exploration trajectory, todo state, evidence receipt, and first edit—not only a
prose plan.

One run per arm is plumbing evidence, not a pass-rate comparison. Freeze exact
model versions and report repeated runs and variance. Each arm aggregate reports
the arithmetic mean and sample variance for input tokens, output tokens, and
wall-clock duration, both overall and per task. The mean is `null` without a
completed adapter result, and sample variance is `null` with fewer than two
samples. Adapter failures, adapter timeouts, validation failures, and validation
timeouts are recorded per run instead of aborting the remaining experiment.
The manifest contract remains schema version 1; reports containing these
run-status and aggregate fields use report schema version 2.

To validate the manifest, worktree, adapter, and success-command plumbing before
using provider credentials, build and pass the included dry-run adapter:

```bash
cargo build --release --example model_ab_dry_run_adapter
cargo run --release --example model_ab -- \
  --manifest target/model_ab.json \
  --adapter target/release/examples/model_ab_dry_run_adapter \
  --output target/model_ab-dry-run-report.json
```

The dry-run adapter does not call a model, edit the worktree, or report token
usage. A passing success command therefore validates only the experiment
plumbing; it is not task-success, quality, or cost evidence.

## Ranked-region interchange

`ranked_region_benchmark` separates retriever output from evaluator labels with
a versioned JSONL contract. Task records contain the repository URL, pinned
40-character revision, path style, query, language and strata, exact budget,
relevant files, and evaluator-only core and optional regions. Prediction
records bind themselves to the exact manifest BLAKE3, repository revision,
budget, and tokenizer, then provide contiguous ranked regions with optional
channel, facet, score, token count, complete response cost, source cost,
latency, and index generation.

The evaluator rejects malformed or absolute paths, unpinned revisions,
noncontiguous ranks, duplicate task IDs, budget overruns, and comparison inputs
with different manifest, dataset, budget, or tokenizer identities. It uses
interval unions for overlapping predictions and reports macro and micro file
recall, line recall/precision/F1, context efficiency, hit/noise region rates,
first useful hit, line-budget NDCG, strata, and token cost per relevant line.

Convert an existing LeanToken representative or validation report into the
same boundary, then score and compare it:

```bash
cargo run --release --example ranked_region_benchmark -- \
  import-representative \
  --manifest benchmarks/validation.json \
  --report target/validation.json \
  --manifest-output target/validation-ranked.manifest.jsonl \
  --predictions-output target/validation-ranked.predictions.jsonl

cargo run --release --example ranked_region_benchmark -- \
  evaluate \
  --manifest target/validation-ranked.manifest.jsonl \
  --predictions target/validation-ranked.predictions.jsonl \
  --output target/validation-ranked.report.json

cargo run --release --example ranked_region_benchmark -- \
  compare \
  --baseline target/baseline-ranked.report.json \
  --candidate target/candidate-ranked.report.json \
  --output target/ranked-ablation.json
```

The SWE-Explore converter accepts a caller-provided JSONL path and optional
JSON object maps from instance ID to issue text and pinned base commit. It does
not fetch data. Keep external datasets outside this repository, verify their
current terms separately from any companion code license, and record the exact
dataset revision and file hash. The checked-in synthetic fixture is
repository-authored and contains no released SWE-Explore task.

Ranked-region metrics measure retrieval against incomplete evaluator labels;
they do not establish patch correctness, model task success, or that every
unlabeled line is useless. Comparisons expose deltas and tradeoffs rather than
emitting a synthetic overall winner.

## Exact MCP wire capture

Before collecting sensitive host traces, generate the deterministic synthetic
exchange to verify that the current catalog, dual result shape, and analyzer
still cover every required wire category:

```bash
cargo run --release --example mcp_wire_fixture -- \
  --output target/wire_trace.synthetic.json

cargo run --release --example mcp_wire_analyze -- \
  --trace target/wire_trace.synthetic.json \
  --output target/wire_trace.synthetic.report.json
```

The fixture is built from `tool_catalog_json` and `tool_result`, not a copied
schema. It is plumbing evidence only: its provider totals are null, it has no
host conversation frame, and its `dual` classification does not prove that any
real host delivers text or structured content to a model.
The checked-in [trace](../benchmarks/wire_trace.synthetic.json) and
[analysis](../benchmarks/reports/wire-trace-synthetic-0.1.1.json) are the
regenerable baseline for LeanToken 0.1.1.
The compact-response fixture records 1,984 complete wire tokens: 1,550 for the
five-tool catalog and 240 for the dual context result. Omitting empty optional
collections reduced that result from 257 tokens without changing the dual mode,
non-empty evidence, receipt hashes, freshness, or range metadata. These local
tokenizer counts remain synthetic serialization evidence, not host or provider
compatibility evidence.

A real Codex CLI 0.144.5 capture initialized LeanToken, listed the five-tool
catalog, and completed `leantoken_files` and `leantoken_read` calls. Its nine
JSON-RPC messages contained 2,896 local tokens: 1,554 for the catalog and 607
for two dual tool results. The archived
[analysis](../benchmarks/reports/wire-trace-codex-cli-0.144.5.json) verifies all
required exchange categories and records two dual results. Codex did not expose
provider-native input totals through the MCP exchange, so those remain null.
The first real-host attempt also showed that hosts may terminate a proxy rather
than close stdio cooperatively; the capture proxy now atomically persists after
every message so such sessions retain evidence.

Place the stdio proxy where the host would normally launch LeanToken:

```bash
cargo run --release --example mcp_wire_capture -- \
  --output target/codex-wire.json \
  --host codex \
  --host-version VERSION \
  -- leantoken --root /path/to/repo mcp
```

The proxy forwards bytes unchanged and records each newline-delimited JSON-RPC
payload in both directions. It writes only protocol bytes to stdout. Analyze
the trace:

Wire traces can contain repository source, prompts, paths, and host metadata.
Store them with the same access controls and retention policy as the repository;
do not publish a raw trace merely to support a token-cost claim.

```bash
cargo run --release --example mcp_wire_analyze -- \
  --trace target/codex-wire.json \
  --output target/codex-wire-cost.json
```

The analyzer separates initialization, tool schemas, calls, results, and
handoffs. When the host exports a provider-native turn total, put it in the
trace-level `provider_total_input_tokens`. Optional event values are deltas used
only for category attribution, never repeated cumulative totals. Partial event
totals remain null. A host-specific conversation frame or model handoff is
outside stdio and must be appended as a `handoff` event if the host exposes it.

## Result compatibility matrix

| Mode | Text content | Structured content | Status |
| --- | --- | --- | --- |
| `dual` | yes | yes | Compatibility default; contains both representations, highest token cost |
| `text` | yes | no | Use only after the host trace proves text consumption |
| `structured` | no | yes | Use only after the host trace proves structured consumption |

Unit tests verify all three serialized shapes; the Rust MCP SDK integration test
covers the default dual mode. This does not prove that every supported host
inserts structured-only results into its model conversation. Capture each
host/version before changing its configured mode, and keep a token-cost snapshot
with the compatibility result.
