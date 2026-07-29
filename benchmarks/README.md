# Representative context benchmark

This benchmark measures one narrow question: given a natural-language maintenance task and a fixed source tree, how much labeled source evidence does `leantoken.context` retrieve within a token budget?

It does not run a model, edit code, execute project tests, or measure whether an agent can solve the task. The results cannot support claims about patch correctness, pass rate, end-to-end task cost, or plan/prewalk handoffs.

## Corpus and labels

[`representative.json`](representative.json) pins eight maintained repositories at the parent of a real bug-fix commit:

- ripgrep (Rust)
- Flask (Python)
- Express (JavaScript)
- Gin (Go)
- Tokio (Rust)
- Vue (TypeScript)
- Cobra (Go)
- Requests (Python)

The task prompts, discovery queries, relevant-file labels, and line anchors were derived retrospectively from the public future fixes named by `fix_commit`. The source indexed by LeanToken is always `base_revision`, before that fix. This makes the labels reproducible, but it also leaks future knowledge into task construction. These are curated retrieval checks, not a blind benchmark or a simulation of naturally arriving issues.

Line anchors are one-based locations in the pinned base revision. An anchor may identify the nearest existing test neighborhood when the regression test did not yet exist. File recall is the primary relevance measure; anchor coverage is a more demanding diagnostic, not proof that the returned excerpt is sufficient to implement a fix.

[`validation.json`](validation.json) pins a separate set of issues that were open
at the 2026-07-15 freeze. Its prompts and labels were collected from the issue
reports and pinned source without consulting a future patch or proposed PR.
These tasks were subsequently used to tune ranking and range selection, so they
are a prospective validation/development set, not a blind holdout. The runner
embeds the manifest BLAKE3 hash and rejects a checkout at the wrong revision.
Use a separate `target/validation-repos` directory because its pinned revisions
differ from the retrospective development set. The repository URLs and exact
revisions are part of `validation.json`.

[`holdout.json`](holdout.json) is separately sealed for the candidate revision
recorded in that manifest. Its nine open-issue tasks span six languages and five
task shapes. The collection, one-run sealing procedure, runtime-tree check, and
reclassification rule are documented in
[`../docs/measurement.md`](../docs/measurement.md). Do not use validation or
retrospective results to alter this manifest before its frozen candidate is
evaluated.

That holdout was evaluated once on 2026-07-16 and is consumed for its frozen
candidate. The unchanged result is
[`reports/holdout-linux-x86_64-2026-07-16.json`](reports/holdout-linux-x86_64-2026-07-16.json).
Its 36% labeled-file recall and 9% line-anchor recall are negative evidence, not
a savings claim; do not tune against it while continuing to describe it as
unseen.

### Frozen holdout vNext

[`frozen_holdout_vnext_policy.json`](frozen_holdout_vnext_policy.json)
pre-registers the next blind promotion boundary before task selection or label
inspection. It freezes the baseline revision, tokenizer, executor and validation
policy, call/time/source budgets, statistical policy, resource envelopes, and
eleven task families. A valid sealed set has at least 60 tasks; every family
must contain multiple tasks, repositories, languages, and task shapes. The
feature candidate revision is bound separately when the evaluator seals the
artifacts.

The evaluator keeps two JSONL artifacts outside the repository:

- public tasks contain prompts, pinned repositories, family/language/shape
  strata, budgets, provenance, and task-specific success-validator commands;
- owner-readable labels contain only task-bound relevant paths and line
  regions.

`frozen_holdout_vnext seal` validates both artifacts, requires owner-only label
permissions on Unix, and writes a publishable receipt containing hashes and
aggregate strata only. It never copies prompts, gold paths, regions, or source
into the receipt. The output is immutable and a changed/missing task, label,
family, binding, provenance field, or policy fails closed:

```bash
cargo build --release --example frozen_holdout_vnext

target/release/examples/frozen_holdout_vnext seal \
  --policy benchmarks/frozen_holdout_vnext_policy.json \
  --tasks target/frozen-holdout-vnext/tasks.public.jsonl \
  --labels target/frozen-holdout-vnext/labels.private.jsonl \
  --host target/frozen-holdout-vnext/host.json \
  --candidate-revision CANDIDATE_GIT_REVISION \
  --harness-revision "$(git rev-parse HEAD)" \
  --evaluator-revision EVALUATOR_GIT_REVISION \
  --toolchain "$(rustc --version)" \
  --output target/frozen-holdout-vnext/seal-receipt.json

target/release/examples/frozen_holdout_vnext verify-public \
  --policy benchmarks/frozen_holdout_vnext_policy.json \
  --tasks target/frozen-holdout-vnext/tasks.public.jsonl \
  --host target/frozen-holdout-vnext/host.json \
  --receipt target/frozen-holdout-vnext/seal-receipt.json
```

Sealing is evidence infrastructure, not a score. Baseline/candidate execution
and the existing retrieval promotion gate remain separate and must bind the
same sealed receipt. Feature implementers must not receive private labels or
per-task gold-derived diagnostics. Once that access occurs, the entire batch is
consumed diagnostic data and cannot support a blind promotion claim.

## Agent wall-time A/B

[`agent_walltime_ab.json`](agent_walltime_ab.json) freezes a local retrieval
latency diagnostic on the four prospective validation repositories. It compares
a persistent structured-result MCP process with the native commands an agent
would otherwise launch:

- exhaustive case-sensitive text search versus sorted fixed-string `rg`;
- exact line reads versus `sed`;
- one ranked `context` request versus the task's frozen sequence of exact `rg`
  discovery queries.

Search compares every occurrence coordinate and read compares exact content and
line coordinates. A mismatch aborts the run. Discovery and context have
different semantics, so their ratio is diagnostic only. The report retains raw
alternating native-first and LeanToken-first samples, warm p50/p95, cold index
time, MCP startup, database size, payload size, and frozen relevance proxies.
It does not run a model or establish end-to-end agent latency.

Prepare the pinned `validation.json` repositories, commit the harness, then run
the release-only clean-worktree wrapper:

```bash
benchmarks/run_agent_walltime_ab.sh \
  target/validation-repos \
  target/agent-walltime-ab
```

The wrapper builds a detached clean `HEAD`, so unrelated files in the caller's
working tree cannot enter the executable or provenance. The output path must
not already exist. Run the harness tests separately with:

```bash
python3 -m unittest scripts/test_agent_walltime_ab.py
```

The first clean-tree baseline is published as
[`reports/agent-walltime-ab-v1-2026-07-26.json`](reports/agent-walltime-ab-v1-2026-07-26.json)
with a
[`Markdown summary`](reports/agent-walltime-ab-v1-2026-07-26.md). All exact
parity and determinism gates passed. Across the four corpus medians, exhaustive
search added 103 ms, exact reads added 14 ms, and context added 214 ms over the
four frozen `rg` discovery sequences. Native discovery reached all 11 labeled
files; context reached 7. This is a negative local baseline, not evidence that
the MCP calls explain model-scale end-to-end latency.

## Prepare pinned repositories

Run from the LeanToken repository root. The commands fetch both the benchmarked base and the future fix used to audit the labels, then leave each worktree detached at the base revision.

```bash
mkdir -p target/representative-repos

git init target/representative-repos/ripgrep
git -C target/representative-repos/ripgrep remote add origin https://github.com/BurntSushi/ripgrep.git
git -C target/representative-repos/ripgrep fetch --depth=2 origin f55548ba9f24dda192880d4a3da2b52e90f6e194
git -C target/representative-repos/ripgrep checkout --detach 2c23e39e0215397884834c0d3cd5a1620f468d30

git init target/representative-repos/flask
git -C target/representative-repos/flask remote add origin https://github.com/pallets/flask.git
git -C target/representative-repos/flask fetch --depth=2 origin 06ea505ce2b2042af26e96d35ebf159af7c0869d
git -C target/representative-repos/flask checkout --detach 2ac89889f4cc330eabd50f295dcef02828522c69

git init target/representative-repos/express
git -C target/representative-repos/express remote add origin https://github.com/expressjs/express.git
git -C target/representative-repos/express fetch --depth=2 origin 18e5985b8a9d5e8423db0a9121f22bdaecd5b120
git -C target/representative-repos/express checkout --detach 59e205a57a04fced6bb7b8ec0b5dec29461a9996

git init target/representative-repos/gin
git -C target/representative-repos/gin remote add origin https://github.com/gin-gonic/gin.git
git -C target/representative-repos/gin fetch --depth=2 origin 4a3eb31fb15b2a2d78b4bdbe0c31a2c564b1977a
git -C target/representative-repos/gin checkout --detach 293ad7edebb3ae30369288bd6416ca0d78474727

git init target/representative-repos/tokio
git -C target/representative-repos/tokio remote add origin https://github.com/tokio-rs/tokio.git
git -C target/representative-repos/tokio fetch --depth=2 origin f59aae423eaf7131d6923085c1c66b50a49bb4e2
git -C target/representative-repos/tokio checkout --detach dc3a883b99f8255cad5409458be95a0bcec2320c

git init target/representative-repos/vue
git -C target/representative-repos/vue remote add origin https://github.com/vuejs/core.git
git -C target/representative-repos/vue fetch --depth=2 origin 932ddd058d69be9bbd8cd796c89f0d1a4fc128d7
git -C target/representative-repos/vue checkout --detach 31d0f23757afb410c638a9c29d44d76d0944e18f

git init target/representative-repos/cobra
git -C target/representative-repos/cobra remote add origin https://github.com/spf13/cobra.git
git -C target/representative-repos/cobra fetch --depth=2 origin 746ef07158728502482cea9f880a6f4b21ef29a9
git -C target/representative-repos/cobra checkout --detach f2878bab8c96afd6e36968af96343b35dbb82a82

git init target/representative-repos/requests
git -C target/representative-repos/requests remote add origin https://github.com/psf/requests.git
git -C target/representative-repos/requests fetch --depth=2 origin 6f205ff422bccd5e4c4fc0b64c5f3e7df5181db6
git -C target/representative-repos/requests checkout --detach 661970d171d9c3e12e4c789c4768db647d8c4da0
```

If a directory already exists, remove it only if it is a disposable benchmark checkout, or choose a new `--repos-root`. The runner rejects a worktree whose `HEAD` does not equal the manifest's `base_revision`.

## Run

Install `rg` and use a release build so debug-mode timing does not enter the report:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/representative.json \
  --repos-root target/representative-repos \
  --output target/representative_benchmark_report.json
```

The JSON report is the result of record. Keep the manifest, LeanToken revision, platform, and generated report together when comparing runs. Do not compare timing across unlike machines or warm-cache states.

Compare two reports from the same frozen manifest with:

```bash
cargo run --release --example benchmark_ablation -- \
  --baseline target/baseline.json \
  --candidate target/candidate.json
```

The command rejects different manifest hashes so an apparent improvement cannot
come from changing tasks or labels.

## Retrieval promotion gate

Retrieval changes must produce reports from the same frozen manifest and a
machine-readable promotion receipt. New schema-v5 manifests assign every task a
nonempty `task_family`; existing frozen manifests retain their bytes and derive
the family deterministically from the first `task_shape`. Reports aggregate
recall, response cost, and warm latency globally and by family so an aggregate
win cannot hide a family regression. Index time, database footprint, and
process RSS are reported globally because they are corpus/process measurements.

Use the quality track when paired agent evaluation shows a task-success gain:

```bash
cargo run --release --example benchmark_ablation -- \
  --baseline target/baseline.json \
  --candidate target/candidate.json \
  --promotion-track quality \
  --baseline-task-success-rate 0.70 \
  --candidate-task-success-rate 0.74 \
  --baseline-two-turn-provider-input-tokens 120000 \
  --candidate-two-turn-provider-input-tokens 121000 \
  --baseline-follow-up-native-reads 24 \
  --candidate-follow-up-native-reads 20 \
  --baseline-tool-calls 90 \
  --candidate-tool-calls 84 \
  --output target/promotion-receipt.json
```

Use `--promotion-track cost` when success and recall are preserved and the
feature is intended to reduce complete response cost or warm p95 latency. The
gate fails closed unless:

- candidate, returned-file, and line-anchor recall do not regress globally or
  in any task family;
- paired task success satisfies the selected quality or cost rule;
- dead-end fragments, dead-end source, repeated ranges, and exact-hash resends
  do not increase;
- paired follow-up native reads do not increase;
- warm p95 latency, cold indexing, database footprint, and available process
  RSS stay within the predeclared resource envelope; and
- the selected track meets its predeclared task-success and complete provider
  cost/tool-call/latency threshold.

The JSON is emitted and optionally written even when the command exits
nonzero, so CI can always publish the failed scorecard for diagnosis. Baseline
and candidate task-success, complete provider-input, native-read, and tool-call
values must come from the same paired agent evaluation; the retrieval harness
does not invent agent-success or provider-cost proxies. The paired evaluation
owns its sampling and statistical-confidence policy. Promotion thresholds are
repository policy recorded in the receipt, not CLI overrides; changing them
requires a reviewed code change.

## Context concept coverage

[`context_concept_coverage.json`](context_concept_coverage.json) is an
independent label overlay for the prospective validation manifest. It leaves
the frozen source manifest and archived report identity unchanged. Every
concept partitions the source manifest's existing path and line anchors
exactly once; the runner rejects an unknown task, path, anchor, duplicate
assignment, omitted anchor, manifest hash mismatch, or changed dataset kind.

Run the normal validation benchmark with the overlay:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest benchmarks/validation.json \
  --concept-labels benchmarks/context_concept_coverage.json \
  --require-concept-thresholds \
  --repos-root target/validation-repos \
  --output target/context-concept-coverage.json
```

The report separates:

- concepts whose frozen anchors appeared in any generated candidate;
- concepts whose anchors appeared in the selected token-bounded evidence;
- selected-to-candidate concept retention;
- per-task coverage, exact matched anchors, source tokens, complete response
  tokens, and the existing relevance metrics.

`--require-concept-thresholds` writes the complete report before exiting
nonzero. The frozen thresholds are regression floors for this consumed
development set, not a promotion gate or a claim that the context is sufficient
to solve a task. One matched anchor credits a concept, so concept coverage must
be read beside line-anchor recall and returned evidence.

`benchmark_ablation` compares these metrics only when both reports use the same
concept-label BLAKE3. It rejects a labeled/unlabeled pair or different overlays.
The first checked run and decision record are
[`reports/context-concept-coverage-v1-2026-07-26.json`](reports/context-concept-coverage-v1-2026-07-26.json)
and
[`reports/context-concept-coverage-v1-2026-07-26.md`](reports/context-concept-coverage-v1-2026-07-26.md).

[`context_feedback_regressions.json`](context_feedback_regressions.json) freezes
self-hosted natural-language and keyword-heavy formulations for the response
accounting and focus-candidate problems. Each task names one owner
implementation, one behavioral test, and one architecture or contract document
before ranking changes are attempted. The fixture is a versioned retrieval
quality input, not part of the published validation aggregate and not evidence
of model task success. A candidate must report its fixture revision and evaluate
both formulations; do not tune either prompt or its concepts after observing
candidate output without creating a new fixture version.

## Handoff manifest crossover

`handoff_manifest_benchmark` validates the opt-in handoff contract on a
deterministic Git fixture. It checks normal/handoff selection parity, exact
path/line/hash rereads, commit and generation provenance, source exclusion,
caller-state transport, deterministic ordering, and complete response token
accounting. Payload results include zero, largest-one, and all exact rereads,
plus three- and six-fragment crossover probes. Only the eight-fragment zero- and
one-reread cases are adoption gates; the synthetic fixture makes no task-success
or scalability claim.

```bash
cargo run --release --example handoff_manifest_benchmark -- \
  benchmarks/reports/handoff-manifest-v1.json
```

## Ranked-region evaluator

`ranked_region_benchmark` provides a versioned JSONL boundary between retrieval
systems and evaluator-owned labels. It validates pinned repository revisions,
line or source-token budgets, ranked regions, manifest identity, tokenizer
identity, and optional retrieval provenance. Overlapping ranges are measured as
interval unions rather than being charged or credited more than once.

Run the repository-owned deterministic fixture with:

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
  --manifest benchmarks/fixtures/ranked_regions/swe_explore.manifest.jsonl \
  --predictions benchmarks/fixtures/ranked_regions/swe_explore.predictions.jsonl \
  --output target/swe-explore.report.json
```

The checked-in [report](fixtures/ranked_regions/swe_explore.report.json) is a
contract fixture, not external benchmark evidence. The converter accepts only
caller-provided local data and never downloads or vendors SWE-Explore. Record
the source revision, file hash, and applicable data terms for every external
run; see [`../docs/measurement.md`](../docs/measurement.md) for the import and
comparison workflow.

### Sealed multilingual development preparation

`swe_bench_multilingual_prepare` turns a caller-provided export of
[`SWE-bench Multilingual`](https://huggingface.co/datasets/SWE-bench/SWE-bench_Multilingual)
into two separately bound artifacts:

- a public task JSONL containing issue text, repository, pinned base commit,
  language, exact/behavioral stratum, budget, and source-record hash;
- an owner-readable label JSONL containing only patch-derived base-revision
  core/optional regions and patch hashes.

The tool never copies raw gold/test patch fields, hints, or repository source
into either output. It uses a structured unified-diff parser. Removed base
lines are core anchors; a pure insertion uses its adjacent base context; added
files are counted but cannot become base-revision labels. Test-patch,
documentation, snapshot, generated, vendored, and lock-file regions are
optional rather than core evidence.

Keep the source Parquet, JSONL export, public tasks, labels, and repository
license map under ignored `target/`. The checked receipt is the only publishable
artifact. Build the harness from a clean revision and run:

```bash
cargo build --release --example swe_bench_multilingual_prepare

target/release/examples/swe_bench_multilingual_prepare \
  --dataset target/external/swe-bench-multilingual/test.jsonl \
  --source-artifact target/external/swe-bench-multilingual/data/test-00000-of-00001.parquet \
  --source-revision DATASET_GIT_REVISION \
  --source-url "https://huggingface.co/datasets/SWE-bench/SWE-bench_Multilingual/blob/DATASET_GIT_REVISION/data/test-00000-of-00001.parquet" \
  --harness-revision "$(git rev-parse HEAD)" \
  --repository-license-map target/swe-bench-multilingual/licenses.json \
  --require-license-audit \
  --tasks-output target/swe-bench-multilingual/tasks.jsonl \
  --labels-output target/swe-bench-multilingual/labels.sealed.jsonl \
  --receipt-output target/swe-bench-multilingual/receipt.json
```

The license map is a JSON array with one entry per unique selected repository
and base revision: `repository`, `spdx_id`, `source_revision`,
repository-relative `license_path`, `license_file_blake3`, and an HTTPS
revision-bound `source_url`. Required audit mode rejects missing/extra
repository revisions, invalid hashes, duplicate entries, and `NOASSERTION`.

The default selection is fixed before retrieval evaluation: six tasks in each
of C, C++, Go, Java, JavaScript, TypeScript, PHP, Ruby, and Rust, split evenly
between title-locus exact identifiers and behavioral tasks, with at most five
tasks from one repository and a 2,000 `cl100k_base` source-token budget. Each
task and the receipt bind the exact tokenizer; the preparer rejects estimated
token counts. Selection uses only the seed, language, task ID, and public title
stratum; it does not use patch locations or retrieval outcomes.

"Sealed" here means immutable file creation, owner-only label permissions on
Unix, task/label source-record binding, and a public BLAKE3 commitment. It is
not encryption against the local user. Keep the label file unopened until the
runtime candidate, configuration, evaluator, tokenizer, and budget are frozen.
For Gate B, use an independent evaluator and separately access-controlled
labels; this public benchmark can only provide development/Gate A evidence.

Validate every sealed region against its exact GitHub base revision without
printing individual paths or labels:

```bash
cargo build --example artifact_blake3

benchmarks/validate_swe_bench_regions.sh \
  target/swe-bench-multilingual/tasks.jsonl \
  target/swe-bench-multilingual/labels.sealed.jsonl \
  target/swe-bench-multilingual/base-region-validation.json
```

The verifier refuses to overwrite its aggregate receipt, binds both inputs and
its own script with BLAKE3, and commits only a hash of the temporary per-file
content manifest. It requires `bash`, `curl`, and `jq`; individual repository
paths remain inside an owner-only temporary directory and are deleted on exit.

The checked [development-set report](reports/swe-bench-multilingual-development-v1.json)
records the pinned 300-row source, two byte-identical preparation runs, 54
selected tasks across nine languages and 30 repositories, all 54 repository
revision license audits, and successful bounds checks for 950 regions in 144
base-revision files. A separate pinned `pyarrow 25.0.0` comparison found all
Parquet and JSONL records equal. Terraform tasks remain governed by BUSL-1.1;
the report also identifies custom repository license references. No upstream
source or patch is vendored. This accepts the data boundary only; it is not a
Gate A retrieval result.

### Four-arm model evaluation

The corrected 2026-07-21 four-arm experiment completed 36 scheduled runs across
Babel, Caddy, and jq. Its
[pre-run commitment](reports/swe-bench-multilingual-four-arm-v2-commitment.json)
and [publishable result](reports/swe-bench-multilingual-four-arm-v2.json) bind
the frozen tasks, models, tool budgets, binaries, official validator, run order,
raw report, and artifact identities. The prior v1 run is separately recorded as
[aborted](reports/swe-bench-multilingual-four-arm-v1-aborted.json); none of its
seven completed cells were reused after a failed-edit telemetry defect was
found.

Filesystem resolved 6/9 runs and progressive LeanToken resolved 4/9, which
meets the pre-registered negative-primary rule. One-shot resolved 0/9 with seven
adapter failures; prewalk resolved 3/9 with six. These are exploratory results
on public, consumed tasks, not Gate B or evidence for a product change. See
[`../docs/measurement.md`](../docs/measurement.md) for the complete metric and
failure interpretation.

The follow-up [trajectory controls](model_ab_trajectory_v1.json) and
[redacted classification](reports/model-ab-trajectory-v1.json) replay 55
hash-bound artifacts from that experiment. Progressive retrieval reduced
median retrieval calls, source tokens, and broad reads, but fell from 6/9 to
4/9 validated successes and increased median overlapping rereads from 2 to
5.5. None of the runs used `expected_hash` or `known_hashes`. Seven prewalk
artifacts transferred grounded evidence and a validated first edit; the four
with observable executor trajectories made zero post-handoff retrieval calls,
while three executor failures remain unknown. The post-hoc decision is
`no_go`: no tool-description, receipt, next-action, or session-state change is
authorized.

`retrieval_reuse_report` aggregates the same frozen exact-trace runs before any
cross-request cache work. In the progressive arm, 141 retrieval calls contained
4 exact range rereads (2.84%) and 57 overlapping rereads (40.43%). Overlap is
not an LRU hit, and the traces do not contain normalized generation-scoped
primitive keys. The result therefore defers a cross-request LRU and treats the
overlap signal as a reason to inspect request-local batching instead.

```bash
cargo run --release --example retrieval_reuse_report
```

### Dependency and caller signal ablation

The frozen [graph-signal controls](graph_signal_ablation_v1.json) and
[release report](reports/graph-signal-ablation-v1.json) compare the same
lexical/syntax candidate set with exactly one additive signal at a time:
concept-corroborated import neighbors, reverse-dependency boosts, or parsed
references. The run uses the eight pinned retrospective tasks above, three
deterministic repetitions, and the exact normal `ContextResponse` selection
path. Evaluation diagnostics never enter the MCP schema or measured response.

All four arms repeated exactly at aggregate and task level and preserved every
baseline candidate. The lexical/syntax baseline found 14/15 labeled files and
9/41 line anchors. Import expansion generated no corroborated candidates on all
five applicable tasks. Reverse dependency left recall unchanged; 15/17 signal
candidate files were false positives, 2/5 selected signal files were labeled
relevant, and 4/5 applicable tasks had no relevant signal candidate. Parsed
callers found one additional line anchor, but 127/135 signal candidate files
were false positives and only 5/21 selected signal files were relevant; it also
increased dead-end source from 3,141 to 3,812 tokens and complete response cost
from 10,825 to 11,893 tokens.

Across the shared graph-enabled indexes, 1,796/9,808 parsed imports resolved to
an indexed file, no resolved path was dangling, and 8,012 imports (81.7%) were
unresolved. The logical SQLite size was 113,127,424 bytes; WAL and SHM sidecars
are excluded. This is a current index cost envelope, not a causal
graph-disabled comparison. No signal passed the preregistered recall and
precision gates, so the result is `no_go`: retain no new ranking boost and
expose no graph metadata.

Reproduce it from a clean checkout and the pinned repositories with a release
binary:

```bash
cargo run --release --example graph_signal_ablation -- \
  --manifest benchmarks/graph_signal_ablation_v1.json \
  --repos-root target/representative-repos \
  --output target/graph-signal-ablation-v1.json
```

### Frozen multilingual Gate A runner

`swe_bench_multilingual_gate` is the one-shot bridge from the sealed
development data to `ranked_region_benchmark`. Freeze and commit the runtime,
evaluator, baseline, candidate configuration, source-token budget, and the
following two input commitments before running it:

```text
tasks BLAKE3:        68ad229a4c9b496e0880b3eb8d25011dd50ac8edec29d09a6ac16907aaea10fd
sealed-label BLAKE3: 1982d01ae08d2c1f324eb9897589ad7795b9c9a4e0e58dfbb750294cfb54e740
tokenizer/budget:    cl100k_base / 2,000 source tokens
```

Build the evaluator in a clean detached evaluator worktree, record the binary
BLAKE3, and materialize the private evaluator manifest without printing it:

```bash
EVALUATOR=target/release/examples/swe_bench_multilingual_gate

"$EVALUATOR" materialize \
  --tasks target/sbml-dev-v1-final-c/tasks.jsonl \
  --labels target/sbml-dev-v1-final-c/labels.sealed.jsonl \
  --expected-tasks-blake3 68ad229a4c9b496e0880b3eb8d25011dd50ac8edec29d09a6ac16907aaea10fd \
  --expected-labels-blake3 1982d01ae08d2c1f324eb9897589ad7795b9c9a4e0e58dfbb750294cfb54e740 \
  --output target/gate-a/manifest.private.jsonl \
  --receipt-output target/gate-a/materialize.receipt.private.json \
  --evaluator-repository EVALUATOR_WORKTREE \
  --evaluator-revision EVALUATOR_REVISION \
  --evaluator-binary-blake3 EVALUATOR_BINARY_BLAKE3
```

Run each clean, revision-pinned runtime binary in a new arm-specific work root.
The predictor treats the manifest as opaque bytes, indexes each of the 54 exact
base revisions, runs context exactly twice, and rejects non-byte-identical
responses or source-token accounting differences:

```bash
"$EVALUATOR" predict \
  --tasks target/sbml-dev-v1-final-c/tasks.jsonl \
  --expected-tasks-blake3 68ad229a4c9b496e0880b3eb8d25011dd50ac8edec29d09a6ac16907aaea10fd \
  --manifest target/gate-a/manifest.private.jsonl \
  --runtime-binary RUNTIME_BINARY \
  --runtime-binary-blake3 RUNTIME_BINARY_BLAKE3 \
  --runtime-repository RUNTIME_WORKTREE \
  --runtime-revision RUNTIME_REVISION \
  --arm-id baseline \
  --repository-cache target/gate-a/repository-cache \
  --work-root target/gate-a/baseline-work \
  --output target/gate-a/baseline.predictions.jsonl \
  --receipt-output target/gate-a/baseline.receipt.json \
  --evaluator-repository EVALUATOR_WORKTREE \
  --evaluator-revision EVALUATOR_REVISION \
  --evaluator-binary-blake3 EVALUATOR_BINARY_BLAKE3
```

Evaluate baseline and candidate predictions with the same private manifest,
then run `decide`. `decide` invokes and verifies the frozen ranked-region
scoring binary itself; precomputed, unbound reports are not accepted:

```bash
"$EVALUATOR" decide \
  --manifest target/gate-a/manifest.private.jsonl \
  --baseline-predictions target/gate-a/baseline.predictions.jsonl \
  --candidate-predictions target/gate-a/candidate.predictions.jsonl \
  --ranked-evaluator-binary target/release/examples/ranked_region_benchmark \
  --ranked-evaluator-binary-blake3 RANKED_EVALUATOR_BINARY_BLAKE3 \
  --baseline-report-output target/gate-a/baseline.report.json \
  --candidate-report-output target/gate-a/candidate.report.json \
  --output target/gate-a/external-decision.json \
  --evaluator-repository EVALUATOR_WORKTREE \
  --evaluator-revision EVALUATOR_REVISION \
  --evaluator-binary-blake3 EVALUATOR_BINARY_BLAKE3
```

The fixed decision requires two distinct predefined evidence groups to improve
line recall or NDCG and permits at most 5% regression in complete tokens per
relevant line. Equivalent `exact_identifier` and `task_shape` strata count
once. The decision reports conflicting strata and can pass only the external
retrieval component; internal smoke, correctness, tradeoff, model A/B, and
Gate B remain separate gates.

The first frozen run is recorded in
[`reports/swe-bench-multilingual-gate-a-v1.json`](reports/swe-bench-multilingual-gate-a-v1.json).
Its external retrieval and efficiency component passed. Linux native checks,
Windows x86-64 cross-linking, and macOS x86-64/arm64 library cross-compilation
grant research-resource admission, but integration Gate A remains pending
native macOS and Windows checks. Cross-compilation is not native test evidence.
The candidate increased macro
line recall and NDCG while also increasing raw source and complete-response
tokens substantially; PHP regressed, several languages were flat, and absolute
precision remained low. It is development evidence, not a product or market
claim. A separately frozen real-model experiment may follow; Gate B, push, and
pull request remain prohibited at this stage.

The repository includes one [Linux x86-64 result](reports/linux-x86_64-2026-07-15.json) as a transparent development record. It is not a cross-platform result or a release claim; rerun the manifest on the target machine for current timings.

The prospective-validation candidate report for `2c0388d` is
[`reports/validation-2c0388d-linux-x86_64-2026-07-16.json`](reports/validation-2c0388d-linux-x86_64-2026-07-16.json),
with the identical-manifest comparison in
[`reports/validation-2c0388d-ablation-linux-x86_64-2026-07-16.json`](reports/validation-2c0388d-ablation-linux-x86_64-2026-07-16.json).
It improves file and line recall at a 228-token complete first-response JSON
cost and a 43-token complete two-turn JSON cost. This is tuned prospective
validation evidence, not a blind generalization result.

## External retrieval corpora

`external_corpora.json` pins the dataset and target-repository revisions used
to import Semble and Sverklo tasks. The lock also records the upstream license,
prompt and label provenance, supported task families, and known limitations.
The source datasets remain upstream; do not commit generated manifests or
repository clones.

Prepare the pinned dataset checkouts and target repositories:

```bash
git clone https://github.com/MinishLab/semble \
  target/external-corpus-datasets/semble
git -C target/external-corpus-datasets/semble checkout \
  906319556a46bca45d8809b4733e05dd51cd5ba2
git clone https://github.com/sverklo/sverklo-bench \
  target/external-corpus-datasets/sverklo-bench
git -C target/external-corpus-datasets/sverklo-bench checkout \
  a0c3017c819452012fee69cd727913ba50fee865

git clone https://github.com/psf/requests target/external-corpus-repos/requests
git -C target/external-corpus-repos/requests checkout \
  ef439eb779c1eba7cbdeeeb302b11e1e061b4b7d
git clone https://github.com/sverklo/sverklo target/external-corpus-repos/sverklo
git -C target/external-corpus-repos/sverklo checkout \
  eaf17512a462bd5fc9cf282804b7158b80356f1a
```

Convert only the supported labels, preflight the generated manifests, then run
the release benchmark:

```bash
cargo run --release --example external_corpus_adapter -- \
  --lock benchmarks/external_corpora.json \
  semble \
  --source target/external-corpus-datasets/semble \
  --repository requests \
  --output target/external-corpora/semble-requests.json

cargo run --release --example external_corpus_adapter -- \
  --lock benchmarks/external_corpora.json \
  sverklo \
  --source target/external-corpus-datasets/sverklo-bench \
  --output target/external-corpora/sverklo.json

cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/semble-requests.json \
  --repos-root target/external-corpus-repos \
  --output target/external-corpora/semble-requests-report.json \
  --preflight-only
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/sverklo.json \
  --repos-root target/external-corpus-repos \
  --output target/external-corpora/sverklo-report.json \
  --preflight-only

cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/semble-requests.json \
  --repos-root target/external-corpus-repos \
  --output target/external-corpora/semble-requests-report.json
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/sverklo.json \
  --repos-root target/external-corpus-repos \
  --output target/external-corpora/sverklo-report.json
```

The adapter preserves Semble primary file and region labels. It does not
promote secondary or find-related annotations. For Sverklo it imports P1
definition, P2 reference, and P4 dependency-file tasks; name-only P5 tasks are
reported and skipped because they have no file or line ground truth. External
results are diagnostics for frozen experiments. They are not blind evidence
and cannot by themselves promote a ranking change.

The first clean, task-stratified run and adoption decision are recorded in
[`reports/external-retrieval-corpora-v1-2026-07-26.md`](reports/external-retrieval-corpora-v1-2026-07-26.md).

The same lock also pins Agent Retrieval Bench (ARB) dataset revision
`c50401f20c60a8c45da94f2ef785ac9a99a6eb55`. The adapter intentionally accepts
only the `v2_trace2code` release. The lock records its 39,295,446-byte archive
SHA-256, and the adapter verifies the extracted `samples.jsonl` BLAKE3. A
bounded Rust/Python smoke run can be prepared without downloading the complete
benchmark:

```bash
huggingface-cli download eyuansu71/agent_retrieval_bench \
  --repo-type dataset \
  --revision c50401f20c60a8c45da94f2ef785ac9a99a6eb55 \
  releases/v2_trace2code/agent_retrieval_bench_v2_trace2code.tar.zst \
  releases/v2_trace2code/agent_retrieval_bench_v2_trace2code.tar.zst.sha256 \
  --local-dir target/arb
(cd target/arb && sha256sum --check \
  releases/v2_trace2code/agent_retrieval_bench_v2_trace2code.tar.zst.sha256)
tar --zstd -xf \
  target/arb/releases/v2_trace2code/agent_retrieval_bench_v2_trace2code.tar.zst \
  -C target/arb

cargo run --release --example external_corpus_adapter -- \
  --lock benchmarks/external_corpora.json \
  arb-trace2code \
  --source target/arb \
  --sample-id 2d65eeb2c97d07f557b1ddb2 \
  --sample-id 08d24e63e480c9447082ccfb \
  --output target/external-corpora/arb-trace2code-smoke.json

git clone --filter=blob:none https://github.com/clap-rs/clap.git \
  target/arb-repos/arb-clap-rs__clap-e82e1edf76bc
git -C target/arb-repos/arb-clap-rs__clap-e82e1edf76bc checkout \
  e82e1edf76bcbddf5fe53428d297520d76a6a300
git clone --filter=blob:none https://github.com/pallets/click.git \
  target/arb-repos/arb-pallets__click-011b9f9d190c
git -C target/arb-repos/arb-pallets__click-011b9f9d190c checkout \
  011b9f9d190c71310264e6c54bae6259f5e38a9f

cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/external-corpora/arb-trace2code-smoke-report.json
```

To compare the opt-in workflow-evidence contract on the same frozen tasks, run
the baseline and candidate from one revision. The candidate derives evidence
only from the public ARB query/trace; it never reads `root_cause_files`, spans,
or other labels:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-workflow-evidence-baseline.json
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-workflow-evidence-candidate.json \
  --workflow-evidence
cargo run --release --example benchmark_ablation -- \
  --baseline target/arb-workflow-evidence-baseline.json \
  --candidate target/arb-workflow-evidence-candidate.json
```

The bounded Git-history experiment uses the workflow-evidence arm as its
baseline. It examines at most 256 already-local ancestor commits with one
merged pickaxe query and feeds at most four current paths into context:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-history-lane-baseline.json \
  --workflow-evidence
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-history-lane-candidate.json \
  --workflow-evidence --history-lane
```

Lazy object fetching is disabled. Blobless checkouts therefore report an
explicit unavailable lane rather than downloading historical blobs or claiming
zero matches. The frozen smoke decision is recorded in
[`reports/arb-history-lane-v1-2026-07-27.md`](reports/arb-history-lane-v1-2026-07-27.md).

The AST structural experiment also uses the workflow-evidence arm as its
baseline. It parses at most 16 KiB of observed failure traces, derives at most
eight call terms, queries only indexed structural definitions, and supplies at
most two soft focus paths:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-ast-structural-baseline.json \
  --workflow-evidence
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-ast-structural-candidate.json \
  --workflow-evidence --ast-structural-lane
cargo run --release --example benchmark_ablation -- \
  --baseline target/arb-ast-structural-baseline.json \
  --candidate target/arb-ast-structural-candidate.json
```

This lane never reads ARB root-cause labels during discovery. The paired report
must distinguish owner-path discovery from selected evidence: a relevant soft
focus path is not itself a retrieval-quality gain. The frozen decision is
recorded in
[`reports/arb-ast-structural-v1-2026-07-27.md`](reports/arb-ast-structural-v1-2026-07-27.md).

The follow-up orientation-capsule experiment reuses the AST candidate arm. It
adds one routing artifact of at most 128 exact tokens per task without changing
context selection:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-orientation-capsule-baseline.json \
  --workflow-evidence --ast-structural-lane
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-orientation-capsule-candidate.json \
  --workflow-evidence --ast-structural-lane --orientation-capsule
```

Capsule path relevance is scored separately from selected and generated source.
It measures whether the route names a labeled owner, not whether an agent reads
that owner or solves the task. The frozen handoff decision is recorded in
[`reports/arb-orientation-capsule-v1-2026-07-27.md`](reports/arb-orientation-capsule-v1-2026-07-27.md).

The follow-up model trajectory run compares `prewalk` with the same prewalk plus
that capsule on the two already prepared repositories. Clap passed in both arms;
the capsule saved 4,198 retrieval source tokens against a 103-token complete
prompt and removed 3,869 labeled dead-end tokens. Click did not yield a complete
pair: the candidate executor exceeded its frozen tool limit and the baseline
executor violated the native-retrieval contract. The classifier preserves the
partial failure, emits `null` paired deltas, and records `no_measured_win` in the
[machine report](reports/arb-orientation-capsule-trajectory-v1-2026-07-27.json)
and
[decision record](reports/arb-orientation-capsule-trajectory-v1-2026-07-27.md).

The AST structural v2 experiment keeps v1 as its control, but changes the
candidate from soft path focus to one exact owner reservation. Auxiliary terms
only count when they co-occur with a structural owner, the first exact eligible
owner among the two diagnostic paths is capped at 128 source tokens, and that
reservation is charged inside the task budget:

```bash
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-ast-structural-v2-control.json \
  --workflow-evidence --ast-structural-lane
cargo run --release --example representative_benchmark -- \
  --manifest target/external-corpora/arb-trace2code-smoke.json \
  --repos-root target/arb-repos \
  --output target/arb-ast-structural-v2-candidate.json \
  --workflow-evidence --ast-structural-lane-v2
cargo run --release --example benchmark_ablation -- \
  --baseline target/arb-ast-structural-v2-control.json \
  --candidate target/arb-ast-structural-v2-candidate.json
```

Native generated-candidate metrics remain separate from the benchmark-side
owner reservation. On a repeated turn, the owner content hash joins the
progressive request and the composite layer suppresses the sidecar. The bounded
local result is recorded in
[`reports/arb-ast-structural-v2-2026-07-27.md`](reports/arb-ast-structural-v2-2026-07-27.md).

Each repository directory under `target/arb-repos` must match the generated
manifest's `directory` and exact `base_revision`. The adapter preserves the
public ARB query object verbatim as JSON, promotes only `root_cause_files` to
gold files, and uses root-cause spans as optional anchors. Related tests and
hard negatives are not gold. A two-task smoke report is diagnostic only: it is
not representative of all 101 trace2code tasks and must not be presented as an
ARB leaderboard or scalability result.

The frozen baseline and its exact provenance are recorded in
[`reports/arb-trace2code-smoke-baseline-v1-2026-07-27.md`](reports/arb-trace2code-smoke-baseline-v1-2026-07-27.md).
The paired workflow-evidence adoption decision is recorded in
[`reports/arb-workflow-evidence-v1-2026-07-27.md`](reports/arb-workflow-evidence-v1-2026-07-27.md).

## Context utilization telemetry

`context_utilization` attaches to the existing model A/B `tool-trace.json` and
`trajectory.json` artifacts. It isolates ranges returned by
`leantoken_context`, then reports separate observable signals: final-patch or
gold-path relevance, later explicit hash inputs, receipt-bearing follow-ups,
exact/overlap rereads, and ranges with no downstream signal. It does not turn
those signals into a guessed utilization percentage:

```bash
cargo run --release --example context_utilization -- \
  --tool-trace target/model-ab/run/tool-trace.json \
  --trajectory target/model-ab/run/trajectory.json \
  --relevant-path src/parser.rs \
  --outcome success \
  --output target/model-ab/run/context-utilization.json
```

The report binds the classifier source, both input hashes, and their shared run
identity. Relevance is an offline proxy, explicit hash input proves identity
retention rather than reasoning, and rereads represent downstream retrieval
pressure. An absent signal is not labeled unused evidence. Artifact bytes,
calls, events, ranges, and relevance paths are all bounded and fail closed; the
classifier performs no repository scan and writes no production telemetry.

## Measurements

The synthetic `hot_path_bounds` report includes deterministic phase counters
next to elapsed-time diagnostics. Regex counters distinguish trigram candidates,
full-scan chunk loads, and verified chunks. Context counters distinguish query
fan-out, candidate sources, storage batch counts, and raw versus unique
hydration requests. On the 2,000-file development corpus, regex planning
selected one mandatory term and verified zero chunks for an absent literal.
Context produced 80 adaptive requests and 60 enclosing-location requests, with
80 and 60 unique keys respectively. That sample shows no duplicate hydration
work to remove; its four adaptive and three enclosing batches are too small to
justify restructuring candidate generation without broader profile evidence.

The separate deep-read diagnostic exercises explicit ranges at both ends of a
near-default-limit synthetic file:

```bash
cargo run --release --example deep_live_read -- --iterations 100
```

The local 30-sample run used a 2,093,034-byte, 35,069-line file. Complete reads
used one file stream, returned the same 32 requested lines without stale or
truncated status, and measured 15.55 ms shallow versus 16.43 ms deep p50.
Those host-local times are sanity diagnostics, not assertions or release
thresholds.

### OpenClaw real-repository profile

The real-repository diagnostic indexed clean OpenClaw commit
`42515c4f07ea3b02e191d30cf97865d4e6229ef0` outside its source tree. LeanToken
saw 28,560 files, indexed 28,186, and skipped 374. A fresh SQLite index measured
276.4 seconds and produced a 1,781,661,696-byte database. Its new storage
breakdown measured 100.7 seconds of file preparation, 30.7 seconds of
relational insertion, 91.2 seconds for chunk trigram FTS, 25.1 seconds for
reference FTS, 10.0 seconds for word FTS, 7.1 seconds for symbol FTS, 3.6
seconds for commit, and 5.8 seconds for the explicit diagnostic checkpoint.
Preparation and insertion occur inside publication and must not be added to the
other rows. GNU `time` reported 473.2 user seconds, 41.7 system seconds, 178%
average CPU, 125,108 KiB peak RSS, and zero swaps for the complete baseline
index-and-profile run. Cold indexing is CPU- and write-intensive, not
memory-intensive on this corpus.

A second profiled cold run split preparation into summed worker time. Parsing
owned 205.7 seconds, whole-file exact token counting 190.4 seconds, per-chunk
token counting 179.0 seconds, and reads 63.1 seconds. The 641.5 seconds of
worker work overlapped into 170.6 seconds of preparation wall time, so the
existing four-worker pipeline is effective. Exact tokenization is the largest
combined owner.

Five warmed steady-state samples on the final tree used 40,316 KiB peak RSS,
zero swaps, and produced:

- absent planned regex: zero candidate or verified chunks, 15.1 ms p50;
- sparse planned regex: 171 candidate and verified chunks, 91 returned hits,
  50.3 ms p50;
- common planned regex: 15,417 FTS candidates, rejected by the 10,000-candidate
  bound in 14.1 ms p50 after a capped count preflight;
- case-insensitive regex: soundly selected the bounded fallback and rejected
  the 28,186-file corpus;
- realistic context: 280 generated candidates and 849.5 ms p50;
- constraint-heavy context: 38 generated candidates and 803.3 ms p50; its
  overlapping focus/must constraints were deduplicated from four logical
  exact-symbol lookups and 26 repeated rows to two unique names, one storage
  batch, and 13 rows;
- complete shallow and deep reads of a 269,375-byte, 7,134-line TypeScript file:
  6.6 and 5.6 ms p50; the token-truncated whole-file read retained its second
  verification stream and measured 44.5 ms p50.

The realistic context request made 12 adaptive-excerpt, 8 enclosing-symbol, and
4 stored-excerpt batches. All 410 hydration keys were unique within the
request. Diagnostic phases locate the owner in lexical FTS (roughly 0.37–0.46
seconds), not enclosing lookup (6–8 ms), stored hydration (under 1 ms), or
lexical verification (2–3 ms). Request-wide hydration batching would reduce
statement executions without removing meaningful content work.

The controlled 12-request trace made 2,224 primitive calls with 428
unique generation-scoped keys. Its 1,796 exact reuses show that identical
requests have cacheable primitives, but the replay deliberately repeats three
request shapes and is not a production arrival trace. It is evidence for
measuring a byte-weighted primitive cache, not for caching complete context
responses or shipping an LRU yet.

An OpenClaw A/B rejected `columnsize=0` for all four FTS indexes. It reduced the
database by only 1.80%, while realistic context regressed from 1,066 ms to
2,682 ms p50 and repeated reverse-order sampling reproduced the regression.
LeanToken ranks lexical rows with `bm25()`, and SQLite must retokenize
external-content rows on demand when stored column sizes are absent. Dynamic
`fts5vocab` probes for a Zoekt-style rare trigram pair also lost end to end:
frequency lookup cost 8–45 ms and positive candidate sets expanded by 4.4× to
12.4×. SQLite rank-first hydration preserved the top-128 set, order, and exact
score for four queries, but alternated between 31–36% faster and 27–53% slower,
so production BM25 ordering remains unchanged.

A no-allocation alternative Rust tokenizer was 6.1–7.3× faster on exact indexed
OpenClaw paths, but disagreed with the canonical tokenizer on 3,550 files.
Python `tiktoken` matched LeanToken's current counts on every indexed file.
Exact token budgets and context coverage take precedence over that incompatible
speedup. See the full
[storage and retrieval report](reports/openclaw-storage-profile-2026-07-25.md).

Reproduce the matrix with:

```bash
cargo run --release --example real_repository_profile -- \
  --repository /path/to/openclaw --iterations 5
```

For each task, the runner reports:

- cold indexing time and SQLite index size;
- warm `leantoken.context` latency;
- labeled-file recall and line-anchor coverage;
- source tokens returned by LeanToken;
- tokens in the complete serialized LeanToken response;
- full contents of the labeled files as an oracle baseline;
- path-sorted `rg --json` discovery output, bounded by the manifest's per-query line limit;
- repeated-context behavior after supplying the first response's known hashes;
- unlabeled-fragment token cost as a proxy for possible dead-end reads;
- complete second-request and second-response JSON token cost;
- second-response source tokens and `estimated_repeated_range_source_tokens`,
  whose line-proportional estimate covers ranges overlapping the first response
  even when the fragment hashes differ.

Source tokens and serialized protocol tokens are separate measurements. LeanToken's reasons, hashes, receipts, and omission metadata cost tokens even when its source excerpts are smaller. A result must not describe source-only savings as total request savings.

The oracle baseline is intentionally favorable to ordinary file reads: it knows the correct files in advance and pays no cost for choosing them or following dead ends. Conversely, adding `rg` discovery output to that oracle can duplicate text and inflate the baseline, so the report keeps discovery, oracle-file, and combined counts visible rather than hiding them behind one headline. The baseline uses a minimal path/content JSON envelope, while LeanToken emits its real response schema; total-JSON comparisons are conservative diagnostics, not like-for-like protocol benchmarks.

The small representation fixture is an intentional counterexample to using
`leantoken.context` for every turn. In the 2026-07-15 fixture run, context used
329 source tokens and 1,710 complete JSON tokens, while direct reads of
already-known labeled files used 527 source tokens and 1,673 JSON tokens. A
compact tree used 555 JSON tokens. Context returned less source but still cost
slightly more complete JSON than an oracle that already knew the ranges. Agents
should use files, outline, search, and exact reads progressively; context is a
discovery tool, not a mandatory wrapper around known ranges.

The MCP fixture serializes initialization, `notifications/initialized`,
`tools/list`, and one real context call. It reports dual, text-only, and
structured-only result costs separately. These are fixture values, not provider
billing numbers. Use the transparent wire proxy for an actual host trace; see
[`../docs/measurement.md`](../docs/measurement.md).

Catalog size is telemetry, not a product budget. Tool names, descriptions, and
input-field descriptions are part of the model-facing capability contract: an
agent cannot compensate at call time for semantics removed from `tools/list`.
Accordingly, the suite snapshots the complete catalog and checks that every
input field is documented, but does not reject it for crossing an arbitrary
word or token threshold. Any proposed catalog reduction needs model-use evidence
that routing and argument quality are preserved; serialization size alone is
not sufficient.

In the 2026-07-15 fixture run, the five-tool catalog was 1,539 tokens. The same
tool result cost 875 tokens in dual mode, 464 as text only, and 433 as structured
content only. That measures serialization opportunity, not host compatibility;
dual remains the default until a real host trace proves a smaller mode works.

The real Codex CLI 0.144.1 run publishes two redacted artifacts: a
[host lifecycle receipt](reports/codex-host-receipt-0.144.1.json) and its
[local wire analysis](reports/wire-trace-codex-cli-0.144.1.json). The receipt
binds frozen source and binary identities, validates three host/MCP result
correlations, and records cumulative provider usage and compaction without
retaining prompts, arguments, outputs, credentials, IDs, or absolute paths.
The wire report measures catalog and dual-result serialization, but no provider
request frame was available. Neither artifact proves that removing local wire
duplication would reduce provider input.

The checked
[real-host compatibility matrix](reports/host-wire-compatibility-v1.json), its
[decision report](reports/host-wire-compatibility-v1-2026-07-20.md), and the
`host_wire_compatibility` validator bind those Codex artifacts to explicit
`dual`, `text`, and `structured` classifications. Codex CLI 0.144.1 proves
structured-only model consumption for one frozen task, while Claude Code,
Cursor, Gemini CLI, and OpenCode were unavailable in the audit environment and
therefore remain unknown rather than zero. The evidence is not broad enough to
change the global `dual` default.

The frozen
[`mcp-response-ablation-v1`](mcp_response_ablation.json) experiment and its
[2026-07-21 report](reports/mcp-response-ablation-v1-2026-07-21.md) compare 12
response and catalog representations against that compatibility matrix. The
manifest binds the historical JSON report by BLAKE3; current-runtime tests
check its acceptance invariants without rewriting the historical token totals.
The one new runtime change omits the internal task fingerprint from the
serialized receipt: response JSON falls from 574 to 556 exact local tokens and
the complete modeled dual handoff from 4,345 to 4,306. The fixed follow-up adds
no exact resend or overlapping source relative to baseline. Larger candidates
that remove freshness, range, deduplication, or model-readable metadata are
rejected; structured-only remains a Codex CLI 0.144.1 opt-in and `dual` remains
the global default. Provider-native values remain null because the available
receipts do not expose an attributable provider request frame.

The frozen
[`compact-projections-v1`](compact_projection_tasks.json) corpus compares the
default and opt-in `files=paths`, `outline=signatures`, and `search=grouped`
service DTOs. It binds the canonical multilingual fixture, adds one deterministic
64-caller reference workload, requires full membership/concept parity,
verifiable compact coordinates/hashes, zero retry-proxy regression, and a
negative complete-response token delta for every projection. Run it in release
mode with:

```bash
cargo run --release --example compact_projection_benchmark -- \
  --manifest benchmarks/compact_projection_tasks.json \
  --repository-root . \
  --source-revision "$(git rev-parse HEAD)" \
  --output target/compact-projection-report.json
```

The checked machine-readable and decision reports live under
`benchmarks/reports/mcp-response-ablation-compact-projections-v1-*`. The retry
proxy only proves that labeled path/symbol routing and verification data remain
available; it is not a model task-success measurement.

The separate
[`multi_agent_context_pilot.json`](multi_agent_context_pilot.json) manifest and
[`run_multi_agent_context_pilot.sh`](run_multi_agent_context_pilot.sh) runner
exercise one root plus one child under full/native, thin/native, and thin
LeanToken retrieval arms. The redacted family receipt analyzer is
`codex_multi_agent_receipt`; it discovers child rollouts, separates inherited
history from live turns, validates an exact path-and-symbol answer, and reports
provider-native cached/uncached usage plus MCP representation bytes. The pilot
is a visible single-task mechanism check, not a general model benchmark. Its
commands, exploratory results, privacy boundary, and interpretation limits are
documented in [`../docs/measurement.md`](../docs/measurement.md).

The repeated follow-up uses
[`multi_agent_context_suite.json`](multi_agent_context_suite.json) and
[`multi_agent_context_suite_v2.json`](multi_agent_context_suite_v2.json) with
[`run_multi_agent_context_suite.sh`](run_multi_agent_context_suite.sh). Four
previously frozen validation tasks across Python, Go, JavaScript, and Rust are
run under three randomized arms with five repetitions each. The
`codex_multi_agent_suite` example validates the complete redacted receipt set,
computes per-task paired savings and a deterministic stratified bootstrap
interval, applies predeclared gates, and retains redacted run samples for
independent recomputation. The v1 iterative profile is negative evidence; the
turn-bounded v2 context-bundle profile passes every frozen gate. See the
[v1](reports/multi-agent-context-suite-v1-codex-0.144.1.json) and
[v2](reports/multi-agent-context-suite-v2-codex-0.144.1.json) reports and the
measurement guide for the result table and limitations.

## Stdio MCP multi-process resource profile

The Linux-only release experiment starts 1, 4, and 8 stdio MCP processes in
shared-cache and independent-cache A/B/B/A order. It measures cold startup,
files/search/read/context warm rounds, idle CPU, and one forced periodic-poll
fallback. The report includes aggregate and per-operation CPU, wall p50/p95,
RSS, threads, file descriptors, estimated read connections, watcher backend and
admission counters, generation publications, WAL, and follower takeover.
Every process/workload has its own sequential baseline; any complete normalized
response mismatch invalidates the decision.

Reproduce it with the command in
[`../docs/development.md`](../docs/development.md#benchmarks). The committed
[raw JSON report](reports/mcp-multiprocess-resource-v1-2026-07-27.json) and
[decision note](reports/mcp-multiprocess-resource-v1-2026-07-27.md) bind the
release binary hash, fixture size, host observations, and predeclared decision
thresholds. The historical v1 raw report remains the evidence for its 1/2/4
shared-cache experiment. Schema v2 is bounded to 16 processes, 10,000 files,
1,000 functions per file, 1,000 warm rounds, a 60-second idle window, 60,000
polling-probe directories, a 120-second polling observation, and a 300-second
operation timeout. See the
[measurement guide](../docs/measurement.md#stdio-mcp-multi-process-cpu-matrix)
for the complete command and interpretation rules.

## Interpretation limits

- Eight hand-selected fixes are too few for a general performance or quality claim.
- Prompts and queries contain vocabulary learned from the future fixes.
- Relevant-file recall rewards finding a labeled file even if the excerpt omits the decisive line; anchor coverage partially exposes this gap.
- The labels do not prove that every labeled file is necessary or that no unlabeled file is useful. “Dead-end” counts therefore mean unlabeled fragments, not confirmed wasted reads.
- Full-file oracle reads model a strong agent that already knows where to look. Real agents may spend more tokens on search and wrong turns.
- `rg` is a discovery baseline, not a ranked context system. Its output depends on the explicit queries supplied here.
- Repository dependencies are not installed and upstream tests are not run.
- No model consumes either payload, so neither payload's practical sufficiency is established.
- Timing and filesystem-cache effects are machine-dependent.

Negative results belong in the report. In particular, small relevant files may be cheaper to return directly than to wrap in ranked context metadata, and a strict token budget may reduce recall. Do not tune labels, prompts, or budgets after seeing results without recording a new benchmark version.

Model task success and prewalk handoffs use the isolated external-adapter
harness documented in [`../docs/measurement.md`](../docs/measurement.md).
Its schema-v3 manifest freezes seeded arm order, source revisions, artifact
hashes, configuration, tool catalogs, and budgets. Retrieval fixtures must not
be presented as model pass-rate evidence. Adapter schema v3 also binds each run
to an immutable tool trace, trajectory, raw provider-usage receipt, and
harness-captured Git patch; report schema v5 records their BLAKE3 identities and
the frozen task definitions plus post-validation receipts.

## Paired performance regression runner

[`paired_performance.json`](paired_performance.json) defines an opt-in,
same-host comparison for the retrieval hot paths and selected indexing
operations. The runner checks out the requested base and the current `HEAD`
into clean detached worktrees, builds both with Rust 1.95 in release mode, and
alternates execution order as AB/BA. Ten pairs are the default; use 20 when a
performance result will support a release or design claim.

The orchestration deliberately delegates statistical comparison to
[`benchstat`](https://pkg.go.dev/golang.org/x/perf/cmd/benchstat). Benchstat
computes the across-run medians, confidence intervals, and Mann-Whitney A/B
comparison. The samples are collected in counterbalanced pairs to reduce order
bias, but the statistical test itself is Benchstat's independent-sample test,
not a custom paired test.

Install the version pinned by the manifest, then run from a clean committed
checkout:

```bash
go install golang.org/x/perf/cmd/benchstat@v0.0.0-20260709024250-82a0b07e230d

benchmarks/run_paired_performance.sh \
  BASE_REVISION \
  target/paired-performance \
  10
```

The base revision must already implement the same benchmark report schemas.
This prevents an older or incompatible harness from being mistaken for
performance evidence. The `Paired performance` workflow exposes the same
runner through a manual dispatch; it is intentionally absent from ordinary
pull-request CI.

Each sample directory retains the two raw JSON reports, stdout/stderr, and a
provenance record containing the commit, tree, Rust, Benchstat, host, pair, and
execution order. Before timing is compared, the adapter verifies the frozen
fixture and operation counts plus hashes of the complete observable hot-path
responses. A parity mismatch invalidates the run.

Benchstat's CSV is the statistical result of record. The small repository
adapter converts the existing reports to Benchstat input and applies the
manifest's per-operation materiality thresholds. A regression fails only when
the head median exceeds both the relative and absolute thresholds and
Benchstat reports a significant difference. A percentage-only change is
`NOISE`; a material but statistically insignificant change is
`INCONCLUSIVE`. Both remain visible without failing the gate. Do not compare
artifacts from different hosts or edit thresholds after seeing a run.

## Indexing and file-read profile

The indexing profile answers two narrower implementation questions:

- how much work a full metadata scan adds when the index is already current,
  compared with reconciling one known changed path;
- how much repeated live file reads cost with the operating system page cache,
  compared with copying the same bytes from process memory.

Run it in release mode and keep the generated JSON when using the measurements
to make an indexing or cache decision:

```bash
cargo run --release --example indexing_profile -- \
  --files 5000 \
  --file-bytes 8192 \
  --iterations 20 \
  --read-samples 5000 \
  --output target/indexing_profile_report.json
```

The runner creates and removes its own deterministic temporary corpus. It
reports full no-op reconciliation, full and targeted modification, create,
delete, rename, and ignore-control reconciliation, repeated reads from a small
hot set, reads spread across the corpus, in-memory byte copies, read-session
open plus snapshot pinning, pooled checkout plus snapshot pinning, and
generation queries on an already pinned session. Lifecycle
operations call the same path-reconciliation entry point used by watcher
events. Create, rename, and ignore changes measure visibility-delta handling;
importers are reparsed only when a changed path intersects their stored reverse
candidate projection.

For a real repository, pin a clean checkout and pass it with `--repository`.
The profiler resolves its commit and uses LeanToken's ignore-aware discovery to
create a disposable snapshot before making any measurement mutations; it never
writes to the supplied checkout. A dirty checkout is rejected because its HEAD
would not identify the measured corpus.

```bash
git clone https://github.com/tokio-rs/tokio target/profile-repos/tokio
git -C target/profile-repos/tokio checkout --detach \
  9cae638de6dc8dd9779c450201df8c102247a242

cargo run --release --example indexing_profile -- \
  --repository target/profile-repos/tokio \
  --repository-label https://github.com/tokio-rs/tokio \
  --iterations 20 \
  --read-samples 5000 \
  --output target/indexing_profile_tokio_linux.json
```

The schema-version 8 report records the caller-supplied corpus label, exact
revision, ignore-visible file count, total and mean bytes, maximum directory
depth, extension mix, initial discovery/hash-plan/prepare/insert/publication
timings, actual preparation batch file/byte high-water, and the SQLite, WAL, and
SHM logical file sizes immediately after the initial commit. The label is
explicit so the profiler never copies a possibly credential-bearing Git remote
into a report. It also records repeated full-noop phase distributions,
directory rename, semantic ignore visibility, native watcher delivery, and the
final storage footprint. Logical file size is not allocated disk usage. Run the
command under `/usr/bin/time -v` (or the platform equivalent) to capture process
peak RSS beside the JSON. Run the same pinned checkout and command on Linux,
macOS, and Windows before making a cross-platform indexing decision. Keep
negative results: if full discovery is not a material p50 or p95 cost, do not
add an incremental journal or directory invalidation layer.

### Dependency-heavy cold-index matrix

The `cold-matrix` lane measures generation-one construction rather than warm
reconciliation. Every worker sample runs in a fresh subprocess and a fresh
SQLite cache so tokenizer/parser initialization, process RSS, and CPU are not
silently inherited from an earlier arm. The parent uses the mirrored default
order `1,2,4,4,2,1` on one snapshot and one host. It also enforces a hard
subprocess deadline beyond the child's cooperative timeout and cancellation
grace.

Prepare the pinned TileLang corpus, including its recorded dependency
submodules:

```bash
git clone --filter=blob:none --no-checkout \
  https://github.com/tile-ai/TileLang.git \
  target/profile-repos/TileLang
git -C target/profile-repos/TileLang checkout --detach \
  eb31994ad782108d8754b19603b428eca9c1e19d
git -C target/profile-repos/TileLang submodule update \
  --init --recursive --depth 1
```

Commit the profiler first, leave the LeanToken and corpus worktrees clean, and
run the already-built release binary:

```bash
cargo build --release --example indexing_profile
target/release/examples/indexing_profile cold-matrix \
  --repository target/profile-repos/TileLang \
  --repository-label https://github.com/tile-ai/TileLang.git \
  --expected-revision eb31994ad782108d8754b19603b428eca9c1e19d \
  --worker-order 1,2,4,4,2,1 \
  --parity-query TODO,class,matmul,kernel,tvm,TileLang \
  --sample-interval-ms 25 \
  --timeout-seconds 7200 \
  --output target/dependency-heavy-cold-index-v1.json
```

The schema-v1 report binds the LeanToken source revision, executable digest,
release/debug state, toolchain, kernel, host parallelism, corpus revision and
shape. It records exact phase wall times, process user/system/total CPU,
process writes, sampled RSS and main/WAL/SHM high-water, discovery admission,
per-language preparation work, relational insertion, all four FTS rebuilds,
commit/checkpoint, final index shape, and final footprint. Logical-table and
retrieval digests must match across every worker arm. Separate fresh-process
probes request cancellation during preparation, relational publication, each
FTS build, and commit/checkpoint, then rebuild the same cache and require the
complete baseline digest.

The decision policy is frozen in the report before execution: preparation must
own at least 35% of leaf-phase time; a candidate must reduce median wall time by
at least 20% without increasing CPU or peak RSS by more than 25%, writes by more
than 5%, or final footprint by more than 5%. Missing Linux resource fields,
missed cancellation phases, timeout, parity differences, or restart differences
fail a decision run. A passing arm is only a follow-up candidate; this lane
never changes the production worker default. The corpus is manual because its
dependency tree is too expensive for ordinary pull-request CI.

The completed
[TileLang Linux x86-64 decision](reports/dependency-heavy-cold-index-tilelang-linux-x86_64-2026-07-28.md)
and its
[raw schema-v1 report](reports/dependency-heavy-cold-index-tilelang-linux-x86_64-2026-07-28.json)
select two workers only as a follow-up candidate. Four workers failed the CPU
and RSS limits. The run used fresh processes and databases, but did not evict
the corpus from the operating-system page cache, so it is not cold-disk
evidence.

The later
[explicit index-scope mechanism profile](reports/index-scope-tilelang-linux-x86_64-2026-07-29.md)
uses the same pinned TileLang revision to compare a fresh full cache with an
opt-in cache excluding `3rdparty/**`. It records the avoided membership,
source, CPU, RSS, and SQLite work plus exact first-party read parity. That
single-host profile supports explicit scope as a user-selected boundary; it
does not change or make a latency claim for default full-repository indexing.

The repository includes one transparent [Tokio Linux x86-64 profile](reports/indexing-tokio-linux-x86_64-2026-07-16.json).
It is a single-host measurement, not a cross-platform conclusion. On that run,
full no-op reconciliation was 28.4 ms p50 / 30.1 ms p95, targeted modification
was 9.8 ms p50 / 15.4 ms p95, and warm file reads were 8.7–12.3 µs p50. Those
absolute read costs do not justify a process-local hot-file cache. That archived
schema-version 2 report predates lifecycle measurements; other operating
systems still need measurement before an incremental-index redesign.

The [streaming-publication Linux profile](reports/indexing-stream-publication-linux-x86_64-2026-07-20.md)
records the exact pre-change revision, same-host RSS comparison, batch
high-water, near-per-file-limit corpus, and pinned Tokio validation used for the
bounded-publication change. It likewise does not replace cross-platform runs.

The [OOM release gate](reports/oom-release-gate-linux-x86_64-2026-07-20.md)
maps the umbrella incident scenarios to executable tests and records final-stack
RSS, SQLite/WAL growth, abrupt leader termination, and same-root multi-process
evidence. Its incident memory figures and controlled synthetic profile are
reported separately because they are not comparable corpora.

The pinned cross-platform monorepo reconciliation matrix is defined in
[`monorepo_reconciliation.json`](monorepo_reconciliation.json). Its separate
workflow runs the release profiler against Tokio and Vue core on Linux, macOS,
and Windows, retaining the raw schema-v8 profile, process-memory receipt, and
stdout for each corpus/platform pair. The completed decision report below keeps
its archived schema-v7 evidence readable. The adoption threshold is frozen in
the manifest before results are collected: a changed-path journal or directory
invalidation prototype is eligible only if full fallback reaches 250 ms p95,
discovery plus hash/planning accounts for at least 50% of that work, and the
result repeats in at least two corpus/platform pairs.

The completed [cross-platform decision report](reports/monorepo-reconciliation-v1-2026-07-20.md)
records the six-pair matrix and its frozen no-go result. No pair reached the
absolute full-fallback threshold, so targeted reconciliation with bounded full
fallback remains the selected design. The expensive workflow is manual after
publishing that result.

Run one frozen pair locally with the already-built release binary:

```bash
cargo build --release --example indexing_profile
target/release/examples/indexing_profile \
  --repository target/profile-repos/tokio \
  --repository-label https://github.com/tokio-rs/tokio.git \
  --iterations 10 \
  --read-samples 1 \
  --hot-set 1 \
  --watcher-debounce-ms 50 \
  --output target/monorepo-profile/profile.json
```

The `Monorepo reconciliation profile` workflow derives all six matrix entries
from the manifest, measures process RSS with the native runner mechanism, and
uploads one raw artifact per pair. After downloading those artifacts without
merging their directories, reproduce the decision with:

```bash
cargo run --release --example monorepo_reconciliation_report -- \
  --manifest benchmarks/monorepo_reconciliation.json \
  --artifacts target/monorepo-profile-artifacts \
  --output target/monorepo-reconciliation-report.json
```

The aggregator requires every expected pair, exact corpus and runtime
revisions, a clean release build, schema v7 or v8, and the manifest's sample
policy. Missing or mixed evidence is an error, not an incomplete decision.

A five-sample schema-version 3 development run on the same pinned Tokio tree
initially measured median create, rename, and ignore-change rebuilds at 21.1 s,
13.5 s, and 29.9 s because each reparsed all 865 indexed files. After replacing
that fallback with visibility deltas and affected-importer resolution, the same
scenarios measured 226 ms, 89 ms, and 49 ms. The create sample indexed one file;
rename indexed one and removed one; a comment-only ignore change indexed only
`.gitignore`. These are small, machine-specific runs, not stable latency or
cross-platform claims. The affected-importer path preserves the case where a
newly visible file resolves imports in an otherwise unchanged file.

Do not infer cold-disk or network-filesystem behavior from this profile. A hot
file cache is justified only when live reads are a material share of measured
end-to-end latency on target repositories and filesystems. The in-memory number
is an upper bound on avoidable read work: it excludes lookup, eviction,
invalidation, synchronization, and memory-pressure costs.

## Cross-platform live-read decision

[`live_read_decision.json`](live_read_decision.json) freezes the Flask and Vue
core revisions, sample policy, and hot-cache adoption threshold before the
cross-platform matrix runs. The experiment profiles a deliberately cache-friendly
repeated eight-file working set plus reads spread across each corpus. It records
direct whole-file reads, in-memory copies, the actual `Services::read` path,
response serialization, a retained and page-touched 256 MiB pressure condition,
process peak RSS, and live-change generation correctness.

The completed [decision report](reports/live-read-decision-v1-2026-07-20.md)
records the six-pair matrix and its frozen no-cache result. No pair met either
the absolute latency or request-share threshold, so LeanToken retains bounded
live reads and the operating-system page cache. The expensive workflow is
manual after publishing that result.

A 64 MiB byte-bounded LRU prototype is eligible only if direct-read p95 reaches
1 ms, direct live reads consume at least 10% of mean service-plus-serialization
time, and both conditions repeat in at least two corpus/platform pairs. Direct
read time is treated as fully avoidable even though a real cache would retain
lookup, cloning, synchronization, eviction, and invalidation costs. This makes
the decision conservative in favor of a cache.

Run one pair from a clean pinned checkout with a release binary:

```bash
cargo build --release --example live_read_profile
target/release/examples/live_read_profile \
  --repository target/live-read-repos/flask \
  --repository-label https://github.com/pallets/flask.git \
  --iterations 200 \
  --hot-set 8 \
  --max-tokens 512 \
  --pressure-bytes 268435456 \
  --output target/live-read-profile/profile.json
```

The `Live-read cache decision` workflow runs the two corpora on Linux, macOS,
and Windows and retains one profile, process-memory receipt, and stdout artifact
per pair. Reproduce its strict aggregate decision after downloading the six
artifact directories without merging them:

```bash
cargo run --release --example live_read_decision_report -- \
  --manifest benchmarks/live_read_decision.json \
  --artifacts target/live-read-profile-artifacts \
  --output target/live-read-decision-report.json
```

The checkout copy and initial index touch every corpus file, so the first
profile pass is not called a cold-cache measurement. The pressure buffer proves
that process memory is retained and touched, not that every operating system
evicted the same page-cache entries. No model runs in this profiler; direct-read
share is compared only with local service and serialization time, making it an
upper bound for an agent request. Remote, encrypted, antivirus-heavy, and
contended filesystems need an in-situ frozen run before a scoped cache decision.
