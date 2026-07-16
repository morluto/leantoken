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

### Adaptive evidence portfolio experiment

Phase 2 candidate `6e08191` adds bounded technical-atom facets, range-scoped
provenance fusion, conservative evidence roles, portfolio selection, and
strictly token-bounded declaration excerpts. The runtime MCP catalog and
response schema are unchanged.

On the consumed four-task validation set, the candidate tied the current
baseline at 8/11 relevant files and 17/38 line anchors. It reduced source
tokens from 3,266 to 3,101, complete first-response JSON from 5,797 to 5,514,
and complete two-turn JSON from 12,410 to 11,358. Ranked-region precision and
F1 improved, but macro file recall and NDCG declined. Because the required
line-anchor result was strictly greater than 17/38, the internal gate failed.

An additional regression run copied the already revealed holdout into
prospective-validation semantics. It is not a second blind run. The nine tasks
span six languages. Against fork `main`, labeled-file recall rose from 7/25 to
14/25, but line-anchor recall fell from 8/111 to 5/111. Macro line recall fell
44.7%, NDCG fell 32.4%, and complete tokens per relevant returned line rose
56.2%. This fails the exact-identifier and evidence-economy gates by a wide
margin. The available 848-record SWE-Explore file was not used as a runtime
pilot because it lacks issue-text and base-commit companion maps and cannot
provide a six-language sample.

The experiment therefore remains a draft, is not enabled on `main`, and does
not advance to compact wire or model A/B work. The exact `IndexNotReady`
regression fixture does avoid expansion noise: at a 700-token source budget it
returns one 122-token implementation range and no test or documentation
fragment. That narrow success does not offset the multi-language line-ranking
regression.

The complete decision record, artifact hashes, failure classification, and
verification counts are archived in
[`../benchmarks/reports/adaptive-evidence-portfolio-phase2-linux-x86_64-2026-07-16.json`](../benchmarks/reports/adaptive-evidence-portfolio-phase2-linux-x86_64-2026-07-16.json).
The development-set candidate and ablation reports are
[`../benchmarks/reports/validation-adaptive-evidence-portfolio-linux-x86_64-2026-07-16.json`](../benchmarks/reports/validation-adaptive-evidence-portfolio-linux-x86_64-2026-07-16.json)
and
[`../benchmarks/reports/validation-adaptive-evidence-portfolio-ablation-linux-x86_64-2026-07-16.json`](../benchmarks/reports/validation-adaptive-evidence-portfolio-ablation-linux-x86_64-2026-07-16.json).

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

`ranked_region_benchmark` provides a versioned JSONL boundary between a
retriever and an evaluator. Task records contain the repository URL, pinned
40-character revision, path style, query, language and strata, exact budget,
relevant files, and evaluator-only core and optional regions. Prediction
records bind themselves to the exact manifest BLAKE3, revision, budget, and
tokenizer, then provide contiguous ranked regions with optional channel, facet,
score, token count, complete response cost, source cost, latency, and index
generation.

The evaluator rejects malformed or absolute paths, unpinned revisions,
manifest/revision/budget mismatches, duplicate or noncontiguous ranks, mixed
dataset/budget/tokenizer reports, and source-token overspend. Windows path
normalization is explicit per task; a backslash in a POSIX task remains a
different path. Interval unions prevent overlapping predictions from
double-counting returned lines or budget.

Run the repository-owned synthetic fixture:

```bash
cargo run --release --example ranked_region_benchmark -- \
  convert-swe-explore \
  --dataset benchmarks/fixtures/ranked_regions/swe_explore.synthetic.jsonl \
  --issue-map benchmarks/fixtures/ranked_regions/swe_explore.issue_map.json \
  --commit-map benchmarks/fixtures/ranked_regions/swe_explore.commit_map.json \
  --output target/swe-explore.manifest.jsonl \
  --line-budget 8

cargo run --release --example ranked_region_benchmark -- \
  evaluate \
  --manifest target/swe-explore.manifest.jsonl \
  --predictions benchmarks/fixtures/ranked_regions/swe_explore.predictions.jsonl \
  --output target/swe-explore.report.json
```

The checked-in prediction is bound to the checked-in converted manifest. If the
manifest is regenerated at another path with identical bytes, its BLAKE3
remains
`4c626913b7920a0d2a8a5efcf9db7189d1491e3d259bfe84399ed95c4f685c1d`.
The deterministic report covers 3/5 core lines in 7 unique returned lines,
with line F1 0.5, NDCG 0.5543214703324495, 35 source tokens, and 120 complete
response tokens.

Convert an existing LeanToken representative or validation report into the
same boundary, then score it:

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

Reports include macro and micro file recall, core-line recall, precision and
F1, context efficiency over core plus optional labels, core-region hit rate,
predicted-region noise rate, first useful hit, line-budget NDCG, strata,
complete response tokens, source tokens, latency, and both token costs per
relevant returned line. Comparisons show baseline, candidate, absolute delta,
and relative delta only when the baseline denominator is nonzero. They do not
emit a synthetic winner score.

The Phase 1 Linux x86-64 delivery record, including hashes, current validation
pilot metrics, wire accounting, schema review, verification, and go/no-go
decisions, is archived in
[`../benchmarks/reports/evidence-economics-v2-linux-x86_64-2026-07-16.json`](../benchmarks/reports/evidence-economics-v2-linux-x86_64-2026-07-16.json).

### SWE-Explore data boundary

The adapter accepts a caller-provided local JSONL path and does not download or
vendor the dataset. The public SWE-Explore JSONL contains line labels but does
not embed issue text or base commits, so use JSON object companion maps
`{instance_id: value}` with `--issue-map` and `--commit-map`. Inline
`problem_statement` and `base_commit` remain accepted for augmented local
records.

Provenance checked on 2026-07-16:

- paper: arXiv `2606.07297`;
- official code: `Qiushao-E/SWE-Explore-Bench` revision
  `3c12dc5a551937038afcbdb6eb6bbf19f3ddd8c1`, MIT;
- Hugging Face dataset repository:
  `SWE-Explore-Bench/SWE-Explore-Bench` revision
  `bdb0ae45d7c337d9e1dc3ebfe2a0af6bc7c1fbd9`;
- dataset card license: `CC-BY-NC-ND-4.0`.

Check the current dataset card and the licenses of referenced repositories
before each use. The code license does not replace the dataset terms. Keep the
download outside this repository, record the exact dataset revision and file
hash in the run artifact, and do not publish converted labels unless their
terms permit it. The checked-in fixture is independently authored and contains
no released SWE-Explore record.

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
schema. It is plumbing evidence only: provider totals are null, only the
synthetic handoff is marked provider-visible, and its `dual` classification
does not prove that any real host delivers text or structured content to a
model.
The checked-in [trace](../benchmarks/wire_trace.synthetic.json) and
[analysis](../benchmarks/reports/wire-trace-synthetic-0.1.1.json) are the
regenerable baseline for LeanToken 0.1.1.

Place the stdio proxy where the host would normally launch LeanToken:

```bash
cargo run --release --example mcp_wire_capture -- \
  --output target/codex-wire.json \
  --host codex \
  --host-version VERSION \
  --provider PROVIDER \
  --model MODEL \
  --repository-revision REVISION \
  --dirty-fingerprint clean \
  -- leantoken --root /path/to/repo mcp
```

The proxy forwards bytes unchanged and records each newline-delimited JSON-RPC
payload in both directions. It writes only protocol bytes to stdout. The proxy
adds a trace ID, canonical content BLAKE3, tokenizer exactness, event sequence,
timestamps, and optional repository identity. It cannot observe turns,
provider framing, cache use, compaction, result visibility, or task outcome
unless the host exports those fields into the trace.

Wire traces can contain repository source, prompts, paths, and host metadata.
Store them with the same access controls and retention policy as the repository;
do not publish a raw trace merely to support a token-cost claim.

```bash
cargo run --release --example mcp_wire_analyze -- \
  --trace target/codex-wire.json \
  --output target/codex-wire-cost.json
```

Trace schema v2 records:

- trace and repository identity;
- model/provider and tokenizer exactness;
- ordered turn/event sequence with optional timestamp and latency;
- exact local JSON-RPC plus separately annotated provider-visible payloads;
- category, tool/call/result IDs, and result visibility;
- generation/path/line/content-hash range identity with source tokens;
- stable-prefix/cache annotations, handoff, and compaction;
- provider-native uncached input, cache creation, cache read, output, and
  reasoning tokens;
- optional outcome.

Unknown provider values remain JSON `null`. Event usage values are deltas, not
repeated cumulative totals. A trace-level provider total is authoritative when
the host exposes one. If one event in a category lacks a provider delta, that
category total remains null. Schema v1 remains readable, but turn, range,
retention, cache, and compaction metrics stay unknown and the report includes a
`legacy_schema` limitation.

The analyzer reports complete serialized JSON-RPC, all explicitly annotated
provider-visible payloads, the handoff subset, their observed-boundary sum,
overlapping component diagnostics, exact
text/structured duplication, range rereads, superseded hashes, stale
generations, first/last visible turns, and serialized/source
`tokens * visible_turn_count`. Component costs overlap their enclosing event
and must not be added to complete-wire cost.

The synthetic v2 trace measures 2,001 serialized JSON tokens and a 13-token
provider-visible handoff, for a 2,014-token observed-boundary sum. The local
and provider boundaries may overlap in real traces, so this sum is not a
provider bill. Its 257-token result remains visible for three turns, producing 771
serialized retained tokens; its 21 source tokens produce 63 retained source
tokens. These are exact `cl100k_base` fixture values, not provider billing.
The report marks provider framing as partial and all provider-native counts as
missing.

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
