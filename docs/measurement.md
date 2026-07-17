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
Evaluation candidate `match_kinds` also include bounded internal provenance in
the forms `facet:<kind>:<fusion-key>` and `channel:<source>:<rank>`. Facets
preserve exact technical atoms and classify symbol, path, behavior, test-intent,
and configuration evidence. This metadata is omitted from production fragment
reasons and does not by itself enable role reservations or portfolio selection;
use it to diagnose a frozen candidate before proposing a scoring change.
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

`model_ab` executes the same frozen task across four required arms:

- ordinary filesystem tools;
- native tools plus a frozen baseline LeanToken runtime;
- native tools plus a frozen adaptive LeanToken runtime;
- adaptive LeanToken for discovery with native-tool recovery.

A frontier prewalk followed by a cheaper executor is an optional fifth arm. It
does not replace any required comparison.

Each run receives a fresh detached Git worktree at the same revision. The
external adapter receives one JSON request on stdin and must return one JSON
result on stdout. For every task and repetition, the harness derives a stable
arm permutation from the manifest's `random_seed`, task ID, and repetition. The
report records the actual permutation and each run's zero-based order index.
Start from `benchmarks/model_ab.example.json`:

```bash
cargo run --release --example model_ab -- \
  --manifest target/model_ab.json \
  --adapter /path/to/provider-adapter \
  --repetitions 5 \
  --artifacts-dir target/model_ab-artifacts \
  --output target/model_ab-report.json
```

The adapter result contract is provider-neutral:

```json
{
  "schema_version": 3,
  "task_success": false,
  "total_input_tokens": 12345,
  "total_output_tokens": 678,
  "provider_reported_cost_usd": 0.42,
  "tool_calls": 12,
  "rereads": 2,
  "reread_tokens": 300,
  "failed_tool_calls": 1,
  "failed_searches": 1,
  "dead_end_reads": 2,
  "provider_usage": {
    "uncached_input_tokens": 9000,
    "cache_creation_input_tokens": 2000,
    "cache_read_input_tokens": 1345,
    "output_tokens": 678,
    "reasoning_tokens": 120
  },
  "evidence_receipt": null,
  "repository_generation": 42
}
```

`task_success` is the agent's own claim; the authoritative report value is
replaced with the frozen success-command result.

The adapter owns provider authentication and the actual model/tool harness.
For request schema v3 it must write `tool-trace.json`, `trajectory.json`, and
`provider-usage.json` into the supplied `artifacts_directory` before returning.
All three use artifact schema v1 and repeat the experiment ID, manifest BLAKE3,
task ID, repetition, and arm supplied by the harness. Tool-trace records have
contiguous sequence numbers, unique call and result IDs, source-token counts,
outcomes, reread markers, and exact repository-relative result ranges with
generation, line bounds, and content BLAKE3. The usage artifact retains the raw
provider receipt beside typed uncached-input, cache-creation, cache-read,
output, and reasoning categories. Use `null` for categories the provider does
not expose; do not infer or replace them with zero.

The harness rejects missing files, binding mismatches, unavailable tools,
invalid ranges, duplicate IDs, and summary counts that cannot be recomputed
from the trace. It captures the run's complete Git diff itself as `patch.diff`,
checks that the patch reverses cleanly, and records every artifact's byte count
and BLAKE3 in the report. Per-run artifact directories are immutable identities,
so a repeated experiment must use a new artifact root or experiment ID.

The harness runs the frozen success command itself after each arm;
agent-reported success is retained only as a diagnostic. The adapter reports
provider input and output tokens, cost, tool calls, rereads, reread tokens,
failed tool calls, failed searches, dead-end reads, raw receipts, and the
observed repository generation. For prewalk, it must transfer the complete
exploration trajectory, todo state, evidence receipt, and first edit—not only a
prose plan.

Manifest schema v2 requires each arm to freeze its clean runtime source
worktree and full revision, runtime binary BLAKE3, adapter source worktree and
revision, adapter binary BLAKE3, configuration, tool catalog, and budget. The
harness verifies every revision and binary digest before the first run and
again before writing the report. The baseline and adaptive runtime sources
must be separate Git worktrees, even when
a dry-run uses the same plumbing binary for both. Prepare and hash frozen
artifacts before writing the final manifest, for example:

```bash
git worktree add --detach target/model-ab-baseline BASELINE_REVISION
git worktree add --detach target/model-ab-adaptive ADAPTIVE_REVISION
cargo run --release --example artifact_blake3 -- \
  /path/to/baseline/leantoken /path/to/adaptive/leantoken \
  /path/to/provider-adapter
```

The harness itself must also run from a clean worktree. Report schema v4 records
its exact revision and executable BLAKE3, the verified arm definitions, random
seed, schedules, and per-run artifact identities. A hash records artifact
identity; it does not establish that two builds used equivalent compilers,
dependencies, or host environments.

One run per arm is plumbing evidence, not a pass-rate comparison. Freeze exact
model versions and report repeated runs and variance. Each arm aggregate reports
the arithmetic mean and sample variance for input tokens, output tokens, and
wall-clock duration, both overall and per task. The mean is `null` without a
completed adapter result, and sample variance is `null` with fewer than two
samples. Adapter failures, adapter timeouts, validation failures, and validation
timeouts are recorded per run instead of aborting the remaining experiment.

To validate the manifest, worktree, adapter, and success-command plumbing before
using provider credentials, build and pass the included dry-run adapter:

```bash
cargo build --release --example model_ab_dry_run_adapter
cargo run --release --example model_ab -- \
  --manifest target/model_ab.json \
  --adapter target/release/examples/model_ab_dry_run_adapter \
  --artifacts-dir target/model_ab-dry-run-artifacts \
  --output target/model_ab-dry-run-report.json
```

The dry-run adapter does not call a model or edit the worktree. It writes the
required empty trace and trajectory plus an explicit zero-usage dry-run receipt.
Use its binary and source identity in every dry-run arm definition. A
passing success command therefore validates only deterministic scheduling,
artifact preflight, isolated task worktrees, adapter invocation, and validation
plumbing; it is not task-success, quality, or cost evidence. Do not use the
example manifest as a formal experiment set.

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
schema. Its schema-v2 envelope includes deterministic turns, one range identity,
three-turn visibility, and one synthetic provider-visible handoff. That handoff
tests accounting only; it is not an observed host conversation frame. Provider
usage remains null, and the `dual` classification does not prove that any real
host delivers text or structured content to a model.
The checked-in [trace](../benchmarks/wire_trace.synthetic.json) and
[analysis](../benchmarks/reports/wire-trace-synthetic-v2.json) are the
regenerable schema-v2 baseline. The current fixture records 2,044 complete
JSON-RPC tokens: 1,610 for the five-tool catalog response and 240 for the dual
context result. The returned range contains 21 source tokens. The complete
result envelope remains visible for three turns, producing 720 serialized
token-turns. Its 13-token synthetic handoff first becomes provider-visible in
turn two and remains visible through turn three, producing 26 provider-visible
token-turns. These values are separate measurements and must not be summed into
a provider billing claim.

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

A second real Codex CLI 0.144.1 session against frozen upstream revision
`73fd764` correlates the private host rollout with an 11-event MCP trace. It
completed `leantoken_files`, `leantoken_read`, and a known-hash repeat read in
one turn; the repeated read returned `not_modified`. The publishable
[host receipt](../benchmarks/reports/codex-host-receipt-0.144.1.json) records
two compactions, five distinct cumulative provider-usage snapshots, and 3/3
semantic matches between host rollout results and MCP responses. The paired
[wire analysis](../benchmarks/reports/wire-trace-codex-cli-0.144.1.json)
records 4,483 local JSON-RPC tokens: 1,595 catalog tokens, 69 call-argument
tokens, 854 result-text tokens, and 776 structured-content tokens. All three
dual results duplicated their structured payload in text, accounting for 776
local tokens.

The host receipt's final cumulative provider accounting contains 7,672
uncached input, 63,232 cache-read input, 282 output, and 87 reasoning tokens;
cache-creation input is unavailable and remains `null`. Each tool result is
followed by a model response and a later provider-usage event in the same turn.
This establishes host lifecycle correlation, not the provider's serialized
request body or per-tool attribution. The MCP analyzer therefore correctly
keeps its provider-usage fields null and marks the exchange local-wire-only.
Without a provider-visible conversation frame, these results do not authorize
compact mode or a provider-saving claim.

Place the stdio proxy where the host would normally launch LeanToken:

```bash
cargo run --release --example mcp_wire_capture -- \
  --output target/codex-wire.json \
  --host codex \
  --host-version VERSION \
  --model MODEL \
  --provider PROVIDER \
  --repository-revision REVISION \
  --dirty-fingerprint FINGERPRINT \
  -- leantoken --root /path/to/repo mcp
```

The proxy forwards bytes unchanged and records each newline-delimited JSON-RPC
payload in both directions. It writes only protocol bytes to stdout and
atomically replaces the trace after every message, so the last complete v2
envelope survives abrupt host termination. Repository revision and dirty
fingerprint are optional but must be supplied together. A transparent stdio
capture cannot infer turns, range identities, compaction, provider framing, or
provider usage; host-specific instrumentation must export those fields.

Wire traces can contain repository source, prompts, paths, and host metadata.
Store them with the same access controls and retention policy as the repository;
do not publish a raw trace merely to support a token-cost claim.

Build a deterministic publishable receipt from a private Codex rollout and its
matching private MCP trace with frozen source and binary identities:

```bash
cargo run --release --example codex_host_receipt -- \
  --rollout PRIVATE_ROLLOUT.jsonl \
  --mcp-trace PRIVATE_MCP_TRACE.json \
  --harness-revision HARNESS_GIT_REVISION \
  --runtime-revision RUNTIME_GIT_REVISION \
  --host-binary /path/to/codex-native-binary \
  --runtime-binary /path/to/frozen-leantoken \
  --capture-binary /path/to/mcp_wire_capture \
  --output target/codex-host-receipt.json
```

The command rejects mismatched host/model/provider identities, invalid MCP
lifecycle order, unresolved or reordered calls, semantic result differences,
cross-turn follow-up evidence, regressing provider usage, unsafe repository
identities, and non-regular frozen artifact paths. It hashes or omits session
and call IDs and never copies prompts, tool arguments, tool outputs,
credentials, or absolute paths into the receipt. Keep both private inputs out
of version control; only the reviewed receipt is publishable.

```bash
cargo run --release --example mcp_wire_analyze -- \
  --trace target/codex-wire.json \
  --output target/codex-wire-cost.json
```

The analyzer validates schema-v2 sequence and content hashes and still reads
schema v1 with explicit legacy limitations. It separates complete JSON-RPC,
source ranges, handoffs, result lifetime, duplicate/reread ranges, changed
hashes, stale repository generations, and provider-native usage. Schema-v2
provider usage has separate uncached input, cache creation, cache read, output,
and reasoning fields. A trace-level usage object is authoritative; event values
must be per-event deltas, never repeated cumulative totals. If any required
event delta is absent, that aggregate remains null. The v1
`provider_total_input_tokens` field remains a generic legacy total and is not
relabelled as uncached input.

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
