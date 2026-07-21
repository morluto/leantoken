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
- progressive narrow LeanToken retrieval with no native repository reads;
- exactly one LeanToken context bundle with no later repository retrieval;
- a frontier LeanToken prewalk followed by a frozen cheaper executor using the
  transferred trajectory, todo state, evidence, patch, and validated first edit.

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
  --preflight-only

cargo run --release --example model_ab -- \
  --manifest target/model_ab.json \
  --adapter /path/to/provider-adapter \
  --repetitions 5 \
  --artifacts-dir target/model_ab-artifacts \
  --output target/model_ab-report.json
```

`--preflight-only` verifies the clean harness, adapter, runtime and task source
revisions, every executable digest, the arm definitions, and validator identity,
then prints a binding receipt without creating run artifacts or invoking the
provider adapter. Run it against the exact final manifest before the first
formal model call.

The adapter result contract is provider-neutral:

```json
{
  "schema_version": 4,
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
For request schema v4 it must write `tool-trace.json`, `trajectory.json`, and
`provider-usage.json` into the supplied `artifacts_directory` before returning.
All three use artifact schema v1 and repeat the experiment ID, manifest BLAKE3,
task ID, repetition, and arm supplied by the harness. Tool-trace records have
contiguous sequence numbers, unique call and result IDs, source-token counts,
outcomes, reread markers, and exact repository-relative result ranges with
generation, line bounds, and content BLAKE3. The usage artifact retains the raw
provider receipt beside typed uncached-input, cache-creation, cache-read,
output, and reasoning categories. Use `null` for categories the provider does
not expose; do not infer or replace them with zero.

The prewalk arm must additionally write `prewalk-handoff.json`. The harness
binds its evidence calls and first validated edit back to the exact tool trace,
requires nonempty trajectory and todo state, and rejects a handoff from any
ordinary arm.

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

Manifest schema v4 requires each arm to freeze its clean runtime source
worktree and full revision, runtime binary BLAKE3, adapter source worktree and
revision, adapter binary BLAKE3, configuration, tool catalog, and budget. The
harness verifies every revision and binary digest before the first run and
again before writing the report. All four arms must use the same runtime source,
binary identity, tool-call budget, and retrieval context budget; only their
frozen retrieval configuration, tool catalog, and the prewalk executor model
differ. Prepare and hash frozen artifacts before writing the final manifest,
for example:

```bash
git worktree add --detach target/model-ab-runtime RUNTIME_REVISION
cargo run --release --example artifact_blake3 -- \
  /path/to/leantoken /path/to/provider-adapter /path/to/validator
```

The harness itself must also run from a clean worktree. Report schema v6 records
its exact revision and executable BLAKE3, the verified arm and task definitions,
random seed, schedules, per-run artifact identities, and validation-receipt
identities. A hash records artifact identity; it does not establish that two
builds used equivalent compilers, dependencies, or host environments.

Schema v4 tasks also require `success_command_executable_blake3` and an absolute
success-command executable. The harness verifies that digest before every run
and before report publication. It supplies the run binding and artifact path in
`LEANTOKEN_MODEL_AB_*` environment variables. The validator must write
`validation-receipt.json`; the harness rejects a receipt written early by the
model adapter, verifies its experiment, manifest, task, repetition, and arm
binding, and records the post-validation receipt identity. This binds the
validator executable, but task-specific arguments and external environments
must still be frozen in the manifest and validator receipt. The success-command
executable identity is checked immediately before every arm's validation and
again before report publication.

`model_ab_codex_adapter` is the concrete Codex CLI adapter. It verifies the
native Codex executable and LeanToken runtime hashes, uses an ephemeral ignored
user configuration, disables web search, fixes reasoning effort and service
tier, and configures only the selected arm's frozen LeanToken MCP server. It
parses official `codex exec --json` events into immutable tool, trajectory, and
provider-usage artifacts. Codex exposes total input, cached input, output, and
reasoning tokens, but not cache-creation input or provider cost; those fields
remain `null`. LeanToken structured results retain exact range identities.
Native shell output has exact local source-token counts but does not expose
reliable repository range identities, so native reread and dead-end metrics are
lower bounds.

The adapter also enforces the retrieval arm from the completed tool trajectory.
Progressive runs must call LeanToken before any substantive command or edit,
receive nonempty narrow-tool evidence, never call `leantoken_context`, and reject
native repository listing, search, and source-read commands. One-shot runs must
make exactly one evidence-bearing `leantoken_context` call before substantive
work and cannot retrieve again. Prewalk applies the progressive restriction to
the frontier phase and forbids repository rediscovery by the executor. Git
status/revision preflight is allowed before LeanToken, and build, test, lint,
and patch-verification commands remain allowed afterward.

The Codex adapter disables parallel tool calls and counts `item.started` tool
events while streaming JSONL. It terminates the child as soon as a run exceeds
its frozen live limit; prewalk and executor phases each receive their own limit
within the common total budget. Completed traces are checked again before
publication. The adapter explicitly approves the frozen local MCP server's
tools because noninteractive `codex exec` otherwise cancels MCP approval prompts
under a workspace-write sandbox. A prompt-only distinction is not sufficient
evidence for an arm. Its `task_success` diagnostic is conservative: at least
one edit must complete and no recorded tool call may fail. The official task
validator still determines report success.

`swe_bench_validator` captures the complete Git patch, runs the pinned official
SWE-bench Docker harness for one instance, and preserves aggregate, instance,
test-output, stdout, stderr, and validation-receipt artifacts. Its receipt binds
the dataset, official harness revision, Python and `uv` executables, frozen
Python package set, Docker image digest, prediction, and official report. A
gold-patch self-test must resolve before model-generated patches are evaluated.

One run per arm is plumbing evidence, not a pass-rate comparison. Freeze exact
model versions and report repeated runs and variance. Each arm aggregate reports
the minimum, median, arithmetic mean, maximum, and sample variance for input
tokens, output tokens, provider cost when exposed, and wall-clock duration, both
overall and per task. It separately reports input-token and duration
distributions for validated successes plus complete-attempt input and provider
cost per success. Aggregate totals are `null` if any run lacks the corresponding
provider result instead of silently dropping failed attempts. Sample variance is
`null` with fewer than two samples. Adapter failures, adapter timeouts,
validation failures, and validation timeouts are recorded per run instead of
aborting the remaining experiment.

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
required empty trace and trajectory plus an explicit zero-usage dry-run receipt
for ordinary arms. For the two-phase prewalk arm it writes an explicitly
synthetic, contract-valid trace and handoff so the stricter transfer validation
also runs without claiming a real edit or provider result.
Use its binary and source identity in every dry-run arm definition. A
passing success command therefore validates only deterministic scheduling,
artifact preflight, isolated task worktrees, adapter invocation, and validation
plumbing; it is not task-success, quality, or cost evidence. Do not use the
example manifest as a formal experiment set.

## Multi-agent context pilot

`multi_agent_context_pilot.json` freezes a small read-only Codex experiment for
one repository-owner tracing task. It measures a root plus exactly one child,
separates full-history and context-free child forks, and compares native reads
with LeanToken dual and structured MCP results. Exact path-and-symbol evidence
is validated from the root's final JSON; the publishable receipt retains only
match counts, topology, provider usage, result sizes, and hashes. Prompts,
answers, tool arguments, tool outputs, credentials, absolute paths, and thread
IDs remain in private local rollouts.

Build the frozen runtime and receipt analyzer, prepare a clean checkout at the
manifest revision, then run one arm. `--execute` is required because the script
performs real model calls. Proxy configuration is inherited from the caller.

```bash
cargo build --release --bin leantoken --example codex_multi_agent_receipt
source ~/clash.sh
benchmarks/run_multi_agent_context_pilot.sh --execute \
  thin-leantoken-structured-owner \
  /path/to/clean/leantoken-checkout \
  target/multi-agent-context-pilot
```

The output directory contains a private Codex stdout/stderr directory, a
redacted JSON receipt, and an SVG with per-thread cached and uncached input.
Never publish the private directory. The runner is intentionally frozen to
Codex CLI 0.144.1; collect a new manifest and compatibility receipt rather than
silently comparing different host versions.

The 2026-07-20 exploratory run produced the following single samples. Exact is
the number of four expected path-and-symbol pairs. MCP bytes are complete
successful/error result JSON measured in the private rollout and retained only
as an aggregate.

| Arm | Exact | Family input | Uncached | Cache read | Output | Child requests | Repository calls | MCP bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| full native | 4/4 | 153,811 | 29,907 | 123,904 | 1,226 | 5 | 4 native | 0 |
| thin native | 4/4 | 141,232 | 23,728 | 117,504 | 1,219 | 6 | 5 native | 0 |
| thin LeanToken dual | 3/4 | 245,668 | 70,052 | 175,616 | 1,292 | 8 | 6 MCP, 1 failed | 72,694 |
| thin LeanToken structured | 2/4 | 147,145 | 38,857 | 108,288 | 1,273 | 6 | 6 MCP | 22,564 |
| thin structured + enclosing owner | 4/4 | 132,624 | 27,152 | 105,472 | 1,042 | 5 | 6 MCP | 38,127 |

The corresponding redacted receipts are archived for
[full native](../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-full-native.json),
[thin native](../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-thin-native.json),
[dual LeanToken](../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-thin-leantoken-dual.json),
[structured LeanToken](../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-thin-leantoken-structured.json),
and the
[enclosing-owner candidate](../benchmarks/reports/multi-agent-context-pilot-codex-0.144.1-thin-leantoken-structured-owner.json).

The full native child inherited 85,738 serialized bytes of parent records. A
context-free fork removed those records and reduced family input by 8.2% and
uncached input by 20.7% despite one additional child request. Codex consumed
structured-only LeanToken results successfully. In the dual run, text and
structured representations contributed 34,656 and 34,564 bytes respectively;
structured mode removed the text copy. This establishes host compatibility and
a serialization mechanism, not a provider-cost reduction.

The first structured run still selected wrapper symbols. Its text search hits
contained the relevant lines but omitted their already-indexed structural
owners. The candidate therefore enriches every lexical text or regex hit with
the narrowest `enclosing_symbol` when one exists. The implementation is shared
across languages; a Rust, Python, and JavaScript fixture verifies the contract.
With that candidate the pilot recovered 4/4 exact evidence, used 6.1% less
family input and 9.4% less child input than thin native, but used 14.4% more
uncached family input. One visible task and one sample cannot establish a
quality, latency, cost, or cross-language win. Repeat randomized arms and add
frozen tasks from several language families before changing Codex setup or the
global `dual` compatibility default.

## Repeated multi-agent context suite

The repeated suite follows up that pilot with four pre-existing prospective
validation tasks from Flask, Gin, Express, and Tokio. The Python, Go,
JavaScript, and Rust repositories are pinned to the revisions already frozen in
`validation.json`; the multi-agent experiment did not create new answer labels.
Path-set success requires the complete labeled file set with no extra path.
This is stricter than file recall but does not require one evidence item per
symbol in a file. It still measures repository triage, not patch correctness.

Each suite runs three arms five times per task, for 60 model runs. A fixed seed
orders the task/arm/repetition Cartesian product by SHA-256 so provider drift
does not systematically favor one arm. Every run uses Codex CLI 0.144.1,
account-selected `gpt-5.6-sol`, low reasoning, read-only sandboxing, one root,
one depth-one child, and the same parent-history calibration. Total input is the
primary context-volume metric. Uncached input is secondary because provider
cache state is not controlled. The deterministic bootstrap resamples the five
repetitions within each fixed task; its interval describes run variability for
these tasks, not generalization to unseen repositories.

Build the runtime and both redaction/aggregation examples, verify the pinned
validation repositories, then run a suite. `--resume` reconstructs a missing
receipt from an existing Codex stdout and skips complete calls instead of
silently rerunning samples. The private directory contains prompts, answers,
tool arguments, and tool outputs and must never be published.

```bash
cargo build --release --bin leantoken \
  --example codex_multi_agent_receipt \
  --example codex_multi_agent_suite
source ~/clash.sh
CODEX_SUITE_MANIFEST=benchmarks/multi_agent_context_suite_v2.json \
  benchmarks/run_multi_agent_context_suite.sh --execute \
  target/validation-repos target/multi-agent-context-suite-v2
```

The first 60-run suite deliberately allowed the structured LeanToken child up
to eight retrieval calls. It rejected the single-sample pilot conclusion:
iterative LeanToken used 50.9% more total input than thin native, lost two path
successes, and was worse in all 20 pairs. The source payload itself was small;
the child averaged 8.2 provider requests versus 4.7 for thin native, so repeated
agent-context framing dominated. Traces also exposed common `range`,
`line_range`, `start_line`, and `end_line` guesses. `leantoken_read` now accepts
those aliases while retaining `lines`, `start`, and `end` as its canonical MCP
schema.

The v2 profile was frozen after inspecting v1. It permits exactly one context
bundle and, only when a required implementation or test file is missing, one
focused search. It forbids iterative file, outline, and read verification for
this file-ownership triage task. Fresh full-native and thin-native controls were
rerun alongside it. The project-scoped
`.codex/agents/leantoken-context-bundle.toml` agent encodes that tested retrieval
contract as an opt-in triage role; implementation agents remain unrestricted.

| Suite arm | Path-set successes | Family input sum | Mean family input | Mean child requests | Contract-violating runs |
| --- | ---: | ---: | ---: | ---: | ---: |
| v1 full native | 13/20 | 3,058,682 | 152,934 | 5.05 | 0 |
| v1 thin native | 13/20 | 2,398,194 | 119,910 | 4.70 | 0 |
| v1 iterative structured LeanToken | 11/20 | 3,619,594 | 180,980 | 8.20 | 13 |
| v2 full native | 13/20 | 3,107,550 | 155,378 | 5.15 | 0 |
| v2 thin native | 12/20 | 2,427,935 | 121,397 | 4.90 | 0 |
| v2 context-bundle LeanToken | 15/20 | 1,939,338 | 96,967 | 3.75 | 0 |

| Paired comparison | Total-input savings | Stratified bootstrap 95% interval | Wins |
| --- | ---: | ---: | ---: |
| v1 thin native vs full native | 21.6% | 15.3% to 27.4% | 18/20 |
| v2 thin native vs full native | 21.9% | 10.1% to 32.3% | 16/20 |
| v2 context bundle vs thin native | 20.1% | 13.4% to 26.3% | 18/20 |
| v2 context bundle vs full native | 37.6% | 32.3% to 43.0% | 20/20 |

The v2 context bundle also reduced uncached input by 16.0% versus thin native
and 27.2% versus full native. Its total-input coefficient of variation was
7.8%, compared with 25.7% and 33.7% for the fresh native controls. All
predeclared v2 gates passed: 75% path-set success versus 60% thin native and
65% full native, positive savings and confidence lower bounds, no per-task
success regression, and no MCP or tool-contract violation. Relative to the v1
LeanToken arm, v2 cut provider requests from 164 to 75 and family input by
46.4%, while increasing successes from 11 to 15.

The two aggregate reports retain all 60 redacted run samples and a hash of the
receipt set, so the summaries can be recomputed without private rollouts:
[v1 iterative baseline](../benchmarks/reports/multi-agent-context-suite-v1-codex-0.144.1.json)
and
[v2 context bundle](../benchmarks/reports/multi-agent-context-suite-v2-codex-0.144.1.json).
The Express path label remained 0/5 for every arm in both suites; the result is
kept as negative dataset evidence rather than relabeled after inspection. Four
fixed development tasks and one host/model/topology are enough to establish a
repeatable mechanism here, not a universal multi-agent or billing claim. Keep
the global MCP result mode at `dual`; the structured-only result path is proven
for this Codex version, not every MCP host.

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

`swe_bench_multilingual_prepare` provides the data boundary for the next
integration Gate A. It accepts a local JSONL export plus the pinned source
Parquet artifact and records both exact-byte identities and an exporter-stable
canonical-record identity. The default deterministic sample contains 54 tasks:
six each for C, C++, Go, Java, JavaScript, TypeScript, PHP, Ruby, and Rust, with
three exact-title and three behavioral-title tasks per language. Raw patches
and evaluator regions remain in ignored local storage; the checked receipt
contains only aggregate label statistics, artifact commitments, source and
harness identities, repository-license evidence, and explicit limitations.
The default 2,000-token source budget is bound to exact `cl100k_base` counting
in every task and in the aggregate receipt.

Gold-patch changed lines are a stronger retrieval proxy than successful-agent
read regions, but they are not causal ground truth. Pure additions are not
retrievable at the base revision, and patch-derived labels can omit unchanged
contracts, callers, and tests needed for a correct fix. Report these limits and
do not treat the development set as task-success or product-generalization
evidence. The public dataset may also be present in model training data, so it
must not be reused as the sealed Gate B holdout.

The preparer creates outputs without overwriting existing artifacts, writes the
label file with mode `0600` on Unix, validates exact/non-exact and repository
quotas, and requires every selected repository license entry when
`--require-license-audit` is used. Its receipt binds the release binary to a
caller-supplied clean Git revision. Reproduce the outputs twice and compare
bytes before accepting a checked receipt; see
[`../benchmarks/README.md`](../benchmarks/README.md) for the command and license
map contract.

The accepted preparation evidence is checked at
[`../benchmarks/reports/swe-bench-multilingual-development-v1.json`](../benchmarks/reports/swe-bench-multilingual-development-v1.json).
It includes a semantic Parquet/JSONL equality check and the aggregate output of
[`../benchmarks/validate_swe_bench_regions.sh`](../benchmarks/validate_swe_bench_regions.sh),
which verified that all 144 unique labeled files exist at their 54 pinned base
revisions and that all 950 core/optional regions are in bounds. This validation
does not reveal individual labels or evaluate retrieval. Freeze the runtime
candidate, evaluator binary, configuration, and prediction contract before the
single Gate A run consumes the development labels.

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

The checked
[machine-readable matrix](../benchmarks/reports/host-wire-compatibility-v1.json)
and [decision report](../benchmarks/reports/host-wire-compatibility-v1-2026-07-20.md)
separate complete JSON-RPC transport, later model activity, provider-native
usage, and provider request framing. Regenerate its validation summary with:

```bash
cargo run --release --example host_wire_compatibility -- \
  --matrix benchmarks/reports/host-wire-compatibility-v1.json \
  --repository-root .
```

| Host/version | Mode | Complete wire | Model consumption | Provider usage | Provider frame |
| --- | --- | --- | --- | --- | --- |
| Codex CLI 0.144.1 | `dual` | proven | proven | partial | unknown |
| Codex CLI 0.144.1 | `structured` | tool results only | one frozen task proven | partial | unknown |
| Codex CLI 0.144.1 | `text` | unknown | unknown | unknown | unknown |
| Codex CLI 0.144.5 | `dual` | proven | unknown | unknown | unknown |
| Codex CLI 0.144.5 | `text`, `structured` | unknown | unknown | unknown | unknown |
| Claude Code, Cursor, Gemini CLI, OpenCode | all | unknown | unknown | unknown | unknown |

The four non-Codex hosts were not installed in the 2026-07-20 audit
environment. This is an access limitation, not an incompatibility result, so
their versions and measurements remain null. The validator rejects substituting
zero for an unavailable measurement and verifies every committed source
artifact by repository-canonical LF JSON BLAKE3 plus its decisive semantic
fields, independent of Windows checkout line-ending conversion.

Unit tests still verify all three serialized shapes, and the Rust MCP SDK
integration test covers the default dual mode. Fixture serialization is not
real-host evidence. Keep `dual` globally until captured compatibility is broad
enough to justify a smaller mode per host/version.

## Frozen MCP response ablation

The checked
[`mcp-response-ablation-v1`](../benchmarks/mcp_response_ablation.json) manifest
binds the canonical `fixtures/sample_repo` tree, one 500-source-token task,
exact `cl100k_base` counting, the host compatibility matrix above, 12
candidates, and zero-additional-reread acceptance gates. Regenerate the
[machine-readable result](../benchmarks/reports/mcp-response-ablation-v1-2026-07-21.json)
with:

```bash
cargo run --release --example mcp_response_ablation -- \
  --manifest benchmarks/mcp_response_ablation.json \
  --repository-root . \
  --output target/mcp-response-ablation.json
```

The accepted change omits the internal task fingerprint from serialized
context receipts. It leaves the in-memory evaluation value, five selected
fragments, 195 source tokens, aligned content hashes, freshness, ranges, and
known-hash behavior unchanged. Exact local response JSON falls from 549 to 531
tokens; the dual result falls from 1,162 to 1,123 and the complete modeled wire
from 3,824 to 3,785. The follow-up exactly resends zero source tokens and its 14
overlapping source tokens are unchanged, so the candidate adds neither exact
resends nor overlapping reads.

Aligned receipt hashes, compact fragment metadata, and omission of empty or
default fields are retained existing compactions. Candidates that remove
freshness, omission accounting, named range fields, tree metadata, readable
reasons, or tool examples fail a correctness or evidence gate. Structured-only
saves 588 local complete-wire tokens but remains scoped to the one proven
Codex CLI 0.144.1 host/task; no global mode changes. Provider-native values are
null because no captured provider request frame supports attribution. See the
[decision report](../benchmarks/reports/mcp-response-ablation-v1-2026-07-21.md)
for the full candidate table and snapshot review.
