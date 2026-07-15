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

The repository includes one [Linux x86-64 result](reports/linux-x86_64-2026-07-15.json) as a transparent development record. It is not a cross-platform result or a release claim; rerun the manifest on the target machine for current timings.

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
`leantoken_context` for every turn. In the current run, context used 561 source
tokens and 3,872 complete JSON tokens, while direct reads of already-known
labeled files used 527 source tokens and 1,673 JSON tokens. A compact tree used
555 JSON tokens. Context was smaller than reading every file it discovered,
but not smaller than an oracle that already knew the answer. Agents should use
files, outline, search, and exact reads progressively; context is a discovery
tool, not a mandatory wrapper around known ranges.

The MCP fixture serializes initialization, `notifications/initialized`,
`tools/list`, and one real context call. With optional output schemas omitted,
the five-tool catalog is 1,364 tokens and the complete modeled handoff is 3,472
tokens, including a 1,882-token result. The result uses the SDK's structured
result, which sends the JSON both as text content and `structuredContent`.
These are fixture values, not provider billing numbers, but they make the fixed
protocol cost visible and regression-tested.

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

## Indexing and file-read profile

The synthetic indexing profile answers two narrower implementation questions:

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
reports full no-op reconciliation, full reconciliation after one file changes,
changed-path reconciliation after one file changes, repeated reads from a
small hot set, reads spread across the corpus, and in-memory byte copies.

Do not infer cold-disk or network-filesystem behavior from this profile. A hot
file cache is justified only when live reads are a material share of measured
end-to-end latency on target repositories and filesystems. The in-memory number
is an upper bound on avoidable read work: it excludes lookup, eviction,
invalidation, synchronization, and memory-pressure costs.
