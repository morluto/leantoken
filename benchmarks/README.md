# Representative context benchmark

This benchmark measures one narrow question: given a natural-language maintenance task and a fixed source tree, how much labeled source evidence does `leantoken_context` retrieve within a token budget?

It does not run a model, edit code, execute project tests, or measure whether an agent can solve the task. The results cannot support claims about patch correctness, pass rate, end-to-end task cost, or plan/prewalk handoffs.

## Corpus and labels

[`representative.json`](representative.json) pins four maintained repositories at the parent of a real bug-fix commit:

- ripgrep (Rust)
- Flask (Python)
- Express (JavaScript)
- Gin (Go)

The task prompts, discovery queries, relevant-file labels, and line anchors were derived retrospectively from the public future fixes named by `fix_commit`. The source indexed by LeanToken is always `base_revision`, before that fix. This makes the labels reproducible, but it also leaks future knowledge into task construction. These are curated retrieval checks, not a blind benchmark or a simulation of naturally arriving issues.

Line anchors are one-based locations in the pinned base revision. An anchor may identify the nearest existing test neighborhood when the regression test did not yet exist. File recall is the primary relevance measure; anchor coverage is a more demanding diagnostic, not proof that the returned excerpt is sufficient to implement a fix.

## Prepare pinned repositories

Run from the LeanToken repository root. The commands fetch both the benchmarked base and the future fix used to audit the labels, then leave each worktree detached at the base revision.

```bash
mkdir -p target/representative-repos

git init target/representative-repos/ripgrep
git -C target/representative-repos/ripgrep remote add origin https://github.com/BurntSushi/ripgrep.git
git -C target/representative-repos/ripgrep fetch --depth=1 origin 2c23e39e0215397884834c0d3cd5a1620f468d30 f55548ba9f24dda192880d4a3da2b52e90f6e194
git -C target/representative-repos/ripgrep checkout --detach 2c23e39e0215397884834c0d3cd5a1620f468d30

git init target/representative-repos/flask
git -C target/representative-repos/flask remote add origin https://github.com/pallets/flask.git
git -C target/representative-repos/flask fetch --depth=1 origin 2ac89889f4cc330eabd50f295dcef02828522c69 06ea505ce2b2042af26e96d35ebf159af7c0869d
git -C target/representative-repos/flask checkout --detach 2ac89889f4cc330eabd50f295dcef02828522c69

git init target/representative-repos/express
git -C target/representative-repos/express remote add origin https://github.com/expressjs/express.git
git -C target/representative-repos/express fetch --depth=1 origin 59e205a57a04fced6bb7b8ec0b5dec29461a9996 18e5985b8a9d5e8423db0a9121f22bdaecd5b120
git -C target/representative-repos/express checkout --detach 59e205a57a04fced6bb7b8ec0b5dec29461a9996

git init target/representative-repos/gin
git -C target/representative-repos/gin remote add origin https://github.com/gin-gonic/gin.git
git -C target/representative-repos/gin fetch --depth=1 origin 293ad7edebb3ae30369288bd6416ca0d78474727 4a3eb31fb15b2a2d78b4bdbe0c31a2c564b1977a
git -C target/representative-repos/gin checkout --detach 293ad7edebb3ae30369288bd6416ca0d78474727
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
- repeated-context behavior after supplying the first response's known hashes.

Source tokens and serialized protocol tokens are separate measurements. LeanToken's reasons, hashes, receipts, and omission metadata cost tokens even when its source excerpts are smaller. A result must not describe source-only savings as total request savings.

The oracle baseline is intentionally favorable to ordinary file reads: it knows the correct files in advance and pays no cost for choosing them or following dead ends. Conversely, adding `rg` discovery output to that oracle can duplicate text and inflate the baseline, so the report keeps discovery, oracle-file, and combined counts visible rather than hiding them behind one headline. The baseline uses a minimal path/content JSON envelope, while LeanToken emits its real response schema; total-JSON comparisons are conservative diagnostics, not like-for-like protocol benchmarks.

## Interpretation limits

- Four hand-selected fixes are too few for a general performance or quality claim.
- Prompts and queries contain vocabulary learned from the future fixes.
- Relevant-file recall rewards finding a labeled file even if the excerpt omits the decisive line; anchor coverage partially exposes this gap.
- The labels do not prove that every labeled file is necessary or that no unlabeled file is useful.
- Full-file oracle reads model a strong agent that already knows where to look. Real agents may spend more tokens on search and wrong turns.
- `rg` is a discovery baseline, not a ranked context system. Its output depends on the explicit queries supplied here.
- Repository dependencies are not installed and upstream tests are not run.
- No model consumes either payload, so neither payload's practical sufficiency is established.
- Timing and filesystem-cache effects are machine-dependent.

Negative results belong in the report. In particular, small relevant files may be cheaper to return directly than to wrap in ranked context metadata, and a strict token budget may reduce recall. Do not tune labels, prompts, or budgets after seeing results without recording a new benchmark version.
