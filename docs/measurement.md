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

## Exact MCP wire capture

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
