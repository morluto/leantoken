# Representative context benchmark

This benchmark measures one narrow question: given a natural-language maintenance task and a fixed source tree, how much labeled source evidence does `leantoken_context` retrieve within a token budget?

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

## Measurements

For each task, the runner reports:

- cold indexing time and SQLite index size;
- warm `leantoken_context` latency;
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
`leantoken_context` for every turn. In the 2026-07-15 fixture run, context used
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

The schema-version 4 report records the caller-supplied corpus label, exact
revision, ignore-visible file count, total and mean bytes, maximum directory
depth, and extension mix. The label is explicit so the profiler never copies a
possibly credential-bearing Git remote into a report. Run the same pinned
checkout and command on Linux, macOS, and Windows before making a cross-platform
indexing decision. Keep negative results: if full discovery is not a material
p50 or p95 cost, do not add an incremental journal or directory invalidation
layer.

The repository includes one transparent [Tokio Linux x86-64 profile](reports/indexing-tokio-linux-x86_64-2026-07-16.json).
It is a single-host measurement, not a cross-platform conclusion. On that run,
full no-op reconciliation was 28.4 ms p50 / 30.1 ms p95, targeted modification
was 9.8 ms p50 / 15.4 ms p95, and warm file reads were 8.7–12.3 µs p50. Those
absolute read costs do not justify a process-local hot-file cache. That archived
schema-version 2 report predates lifecycle measurements; other operating
systems still need measurement before an incremental-index redesign.

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
