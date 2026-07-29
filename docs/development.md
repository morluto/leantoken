# Development and testing

## Prerequisites

LeanToken requires Rust 1.95 or later and a native C/C++ toolchain for bundled
SQLite and tree-sitter grammar crates. On macOS, install Xcode Command Line
Tools. On Windows, install Visual Studio Build Tools.

Install the repository's Git hooks after cloning:

```bash
python -m pip install pre-commit
pre-commit install --install-hooks
```

This installs a commit hook for formatting and inexpensive file validation.
Local hooks deliberately do not compile the project or run tests. The full
lint and test gates run in GitHub Actions, where they do not block commits or
the first push.

An older checkout may still have the retired push-hook wrapper installed. It
can be removed once with:

```bash
pre-commit uninstall --hook-type pre-push
```

## Development checks

The normal edit-and-commit loop does not need to reproduce the complete CI
suite. The installed commit hook runs:

```bash
cargo fmt --all -- --check
```

Run a focused product-test module while developing:

```bash
cargo test-focused services::
```

The argument is an ordinary Rust test-name filter applied to the library,
binary, and integration test targets, so it can also select one exact test. Use
the owning module listed under [Test responsibilities](#test-responsibilities).
When a change crosses ownership boundaries, run each affected filter; when
ownership is unclear, let the full CI suite supply the conservative fallback.

Run the complete product-behavior suite without compiling or executing the
benchmark contract or examples:

```bash
cargo test-product
```

For faster local feedback, run independent unit and ordinary integration
lanes concurrently, then run process-heavy executable and MCP tests with two
workers:

```bash
python3 scripts/test_product_parallel.py
```

This runs the same product tests as `cargo test-product`; process-heavy tests
stay isolated so higher parallelism does not make their startup deadlines
flaky.

Run the token-economy contract explicitly when changing retrieval accounting or
its fixture. CI runs it on every supported OS for every Rust change:

```bash
cargo test-contract
```

This full local command uses two test workers so process-heavy MCP tests do not
multiply child-process load on smaller machines. Focused tests retain Cargo's
normal host parallelism.

Benchmark and example tests are a separate target group because Cargo executes
test binaries serially. Run them when changing `examples/`, benchmark fixtures,
or their shared behavior:

```bash
cargo test-extras
cargo test --all-features --doc
```

These repository-local Cargo aliases keep the fast and extended target groups
consistent with CI. The development profile retains line tables for useful
backtraces in LeanToken while omitting dependency debug information, which
keeps links and the local `target` directory smaller. Override it temporarily
with `CARGO_PROFILE_DEV_DEBUG=2` when a debugger needs full local-variable data
for LeanToken code. When debugging a dependency too, pass
`--config 'profile.dev.package."*".debug=2'` to Cargo.

## Pull request readiness

Before the first push, format the tree and run the smallest relevant check or
behavioral test that proves the change. Open the pull request once that focused
proof passes so the broad checks can run in parallel with review.

GitHub Actions is the merge gate for:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test-product
cargo test-contract
```

CI also runs benchmark, example, and documentation tests once on Linux. On
Linux, macOS, and Windows it runs library and binary unit tests, ordinary
integration behavior, and executable/MCP process behavior as separately timed
phases. The process-heavy phase uses two test workers because each test can
start several child processes; ordinary tests retain the runner's default
parallelism. Rust changes also run the instrumented coverage gate in parallel
(50% line floor; the opt-in `concurrency_profile` harness is excluded).
The token-economy contract runs separately on all three operating systems.
A pull request is not ready to merge until its required CI checks pass.

Repository rules for `main` should require the CI workflow's `Required checks`
job. That stable aggregate check fails when any job relevant to the changed
paths fails, while allowing intentionally skipped jobs to remain conditional.
Requiring the aggregate avoids coupling branch rules to every matrix entry and
path-filtered job name.

Run a complete gate locally when reproducing a CI failure, working without CI,
or changing the gate itself. Otherwise, do not routinely duplicate the full CI
suite before and after the first push. Run rustdoc locally when changing public
APIs or documentation:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Build and verify the distributable crate when changing packaging or features:

```bash
cargo build --release
cargo package
```

While `rmcp` is pinned to the temporary response-task fix for rust-sdk#1026,
`cargo package` is a validation step only. Cargo normalizes git dependencies to
their registry version in a published crate, which would resolve unpatched
`rmcp 2.2.0`. Native release artifacts and the documented `cargo install --git`
path build from the pinned repository revision. Do not publish this crate to
crates.io until the fix is available in a released `rmcp` version and the pin
has been removed.

## Release artifacts

The generated release workflow builds native archives for Linux x64/arm64,
macOS x64/arm64, and Windows x64. A custom packaging job converts those
archives into one `leantoken` npm package containing all five native binaries.
The included JavaScript launcher selects the binary for the current OS and CPU;
npm installation does not run lifecycle scripts or download a binary from a
postinstall hook.

`npm/platforms.json` is the canonical platform manifest: it owns the target
triple and npm launcher metadata. The `targets` projection in
`dist-workspace.toml` is checked against it by the npm packaging test, so add or
remove a platform in the npm manifest and let that check expose any release
configuration drift.

Verify release configuration changes before pushing them:

```bash
dist generate --check
dist plan
```

Test the package generator, including its complete binary layout:

```bash
node --test npm/npm-packaging.test.mjs
```

Test a host-native npm installation with lifecycle scripts disabled, including
the npm command shim, JavaScript launcher, executable selection, and argument
forwarding:

```bash
node --test npm/npm-install-e2e.mjs
```

CI runs the host-native installation test on Linux, macOS, and Windows.

Merging an `autorelease` PR creates a version tag such as `v0.1.0` and
dispatches `.github/workflows/release.yml` with that tag. Keep the Cargo package
version, tag, GitHub release, and npm package version identical.

npm publication uses npm trusted publishing from `.github/workflows/release.yml`.
Configure the `leantoken` package on npmjs.com with `morluto/leantoken` as the
repository and `release.yml` as the workflow filename. The release workflow
checks the package identity and contents before publishing it with provenance.

To inspect a package manually before publication, run:

```bash
tar -xOf leantoken-VERSION.tgz package/package.json
npm publish leantoken-VERSION.tgz --dry-run
```

The dry-run file list must contain one binary for every target in
`npm/platforms.json`, and the manifest must not define lifecycle scripts or
dependencies. For recovery when trusted publishing is unavailable, publish
only after those checks pass:

```bash
npm publish leantoken-VERSION.tgz --access public
```

Confirm the release from the registry rather than a local package or npm cache:

```bash
npm view leantoken@VERSION version
npx --yes leantoken@VERSION --version
```

Prereleases use the npm `next` tag; stable releases use `latest`.

## Test responsibilities

Tests are organized around observable behavior:

Integration test files are modules of one `integration` target so Cargo can
run them in parallel rather than starting one executable per file.

- `tests/storage.rs`: migrations, WAL/foreign keys, FTS5, atomic replacement,
  rollback, stale-plan rejection, reopen, and generation behavior;
- `tests/indexer.rs`: initial, unchanged, changed, deleted, rebuilt, bounded
  chunking, targeted reconciliation, and dependency invalidation;
- `tests/services.rs` registers owner-focused modules under `tests/services/`:
  lifecycle/repository/consistency/path safety, per-tool behavior, limits and
  response budgets, context planning/signals/workflows/diffs, receipts/savings,
  language coverage, and smoke behavior;
- `tests/mcp.rs`: SDK initialization, readiness states, retryable startup tool
  errors, exact tool catalog, structured calls, cancellation, and
  post-cancellation liveness;
- `tests/binary.rs`: CLI JSON flow, concurrent and contended cold-cache MCP
  initialization, runtime-failure visibility, single-leader generation
  publication, leader failover, MCP EOF shutdown, and repository-free episode
  audit behavior through the executable;
- `tests/repository.rs`: ignore behavior, path validation, size limits, symlink
  containment, bounded Git probes, and nested-worktree path normalization;
- `tests/watcher.rs`: event delivery and joined shutdown;
- `tests/benchmark_contract.rs`: token-economy and known-hash regression fixture;
- `tests/mcp_token_costs.rs`: real tool catalog and JSON-RPC handoff accounting;
- `tests/representation_comparison.rs`: tree, outline, search, read, and context
  representation costs.

`src/episode.rs` owns unit coverage for versioned analyzer adapters, published
60-run replay, exact/proxy classification boundaries, binding/privacy failure,
resource caps, and deterministic JSON/Markdown normalization.

Pure parsing, text-range, ranking, tokenization, and watcher state behavior is
covered next to the owning module where private invariants matter.

CI runs the complete suite on Linux, macOS, and Windows. A local Linux pass is
not evidence for native watcher or path behavior on the other platforms; rely
on the matrix before merging portability changes.

## Storage and retrieval changes

Treat the SQLite schema and retrieval ordering as behavioral contracts, not
implementation details. When changing them:

- use a versioned migration and test both a new database and an upgraded one;
- keep multi-query responses inside one `ReadSession` snapshot;
- bind public pagination cursors to the committed generation and operation
  parameters, even when the underlying query uses a simpler keyset;
- preserve deterministic ranking, overlap, and token-budget behavior when
  replacing per-item reads with batched joins;
- record every new fan-out or scan bound in `docs/architecture.md`; and
- collect timing evidence with a release build on a representative corpus.

Any change that alters candidate generation, ranking, context allocation, or
default retrieval signals must also run the
[retrieval promotion gate](../benchmarks/README.md#retrieval-promotion-gate).
Attach its machine-readable receipt to the pull request. A development-set win
is not sufficient: use one frozen manifest for both arms, use explicit task
families for new manifests (legacy frozen inputs derive them from
`task_shape`), supply paired task-success/provider-cost/tool-use metrics, and do
not enable the feature by default when the gate exits nonzero.

Prefer query-plan evidence (`EXPLAIN QUERY PLAN`) and focused integration tests
for storage changes. A faster microbenchmark is insufficient if it weakens
atomic publication, stale-plan rejection, bounded memory, or deterministic
results.

## MCP schema snapshots

The generated eight-tool catalog is snapshot-tested. Review snapshot changes as
protocol changes: tool names, descriptions, required fields, defaults, and
schema size all consume client context or affect compatibility.

Update an intentional snapshot with:

```bash
cargo insta review
```

Do not accept a snapshot solely because generation changed; inspect the schema
diff first.

## Public Rust API compatibility

The crate remains on the `0.x` development line. `Error` is intentionally
non-exhaustive: consumers must include a fallback arm and should only branch on
variants they can recover from. LT-06 establishes that contract while adding
`RequestLimitExceeded`, whose fields are required for adapter-safe limit
reporting. `ResponseBudgetExceeded` separately represents a computed,
exactly-retryable response minimum and exposes only bounded aggregate
accounting. These are acknowledged source-compatibility changes for consumers
that exhaustively matched the earlier enum. Release PRs own package version
changes; feature and fix PRs do not edit `Cargo.toml` versions independently.

`IndexResponse` retains its original constructible field set for downstream
Rust source compatibility. Additive preparation accounting is exposed through
`IndexReport`, returned by the new `Indexer::*_report` and
`Services::*_report` methods. The report flattens the compatible response for
JSON output, so CLI consumers receive `skip_reasons` without forcing existing
Rust consumers to update struct literals or destructuring patterns.

`CacheListRequest`, `CacheListReport`, and `CachePruneRequest` likewise retain
their constructible field sets. Content-compatibility filters and summaries use
`CacheListV2Request`/`CacheListV2Report`; compatibility pruning uses
`CachePruneV2Request`. The CLI selects those versioned APIs while older Rust
callers can continue using the original list/prune methods.

Explicit indexing scope adds public provenance fields to `ResponseMeta`,
`StatusResponse`, and `CacheEntry`, plus the `IndexScopeMismatch` error variant.
This is wire-additive and legacy deserialization defaults to full scope, but
downstream Rust consumers constructing those public response structs with
literals must add the new fields. `IndexScope` is immutable after
normalization; use `Config::discover_scoped` instead of mutating cache
membership after service startup.

Use `InvalidRequest` only for audited caller validation. Infrastructure and
invariant failures use `InternalFailure`, which retains the historical
`invalid request: ...` display prefix for CLI text compatibility while adapters
classify it as internal. Do not infer error categories from rendered strings.

## Benchmarks

The fixture benchmark is an opt-in regression check because it performs a cold
index and several context requests:

```bash
cargo test-contract
```

The representative benchmark requires pinned external worktrees and `rg`. See
[`../benchmarks/README.md`](../benchmarks/README.md) for preparation, command
line, measurements, and interpretation limits.

The frozen validation set, ablation runner, isolated model A/B adapter, and
exact MCP wire proxy are documented in [`measurement.md`](measurement.md).

The same guide documents the synthetic indexing and file-read profile used to
gate targeted reconciliation and any future hot-file cache.

For a generation-one dependency-heavy decision, build
`indexing_profile` in release mode and use its `cold-matrix` subcommand against
the pinned clean TileLang checkout documented in the benchmark guide. The lane
runs 1/2/4-worker screening samples and cancellation/restart probes in isolated
subprocesses. Its `--matrix-kind two-worker-follow-up` mode instead runs four
samples per arm in alternating ABBA/BAAB order and requires both p50 and p95
improvement. Both modes validate complete logical/retrieval parity and write
the raw report under `target/` by default. They are manual evidence and are not
part of `cargo test-extras` or normal pull-request CI; the small in-process
contract test remains in the example test target.

On Linux, reproduce the stdio MCP ownership and resource profile after building
the product binary in release mode:

```bash
cargo build --release
cargo run --release --example mcp_multiprocess_profile -- \
  --binary target/release/leantoken \
  --max-index-workers 1 \
  --process-counts 1,4,8 \
  --files 200 \
  --functions-per-file 40 \
  --warm-iterations 10 \
  --idle-seconds 5 \
  --polling-directories 50001 \
  --polling-observation-seconds 31 \
  --output target/mcp-multiprocess-cpu-v3.json
```

The example rejects more than 16 processes, 10,000 fixture files, 1,000
functions per file, 1,000 warm rounds, 60 idle seconds, 60,000 polling
directories, 120 polling-observation seconds, a timeout above 300 seconds, or
an explicit worker limit outside `1..=64`. It requires Linux `/proc`; use
`--skip-polling-probe` only for a mechanical smoke run. A guarded 1-vs-2
cold-start contention comparison runs the complete profiler four times in
external `1,2,2,1` order and retains every schema-v3 report. Historical raw
artifacts and their interpretation are linked from the benchmark guide; write
new host-local evidence under `target/` unless it is being reviewed as a
versioned benchmark report.

Run the deterministic semantic change receipt gate in release mode:

```bash
cargo run --release --example semantic_change_receipt_benchmark -- \
  --iterations 21 \
  --output benchmarks/reports/semantic-change-receipt-v1.json
```

The gate checks an exact symbol/configuration truth set, configuration-value
non-disclosure, owner-test statuses, complete-response token overhead, and
end-to-end latency. It is a protocol and classifier check, not a model task
evaluation.

For same-host regression checks, use the opt-in paired performance runner
documented in the benchmark guide. It builds clean base/head worktrees with the
pinned Rust toolchain, alternates AB/BA order, validates observable-response
parity, and delegates statistical comparison to pinned Benchstat. Its
comparator contract is part of the normal example-test CI group; the expensive
timing workflow runs only by manual dispatch.

Keep negative results. Do not tune prompts, labels, or budgets after seeing a
result without recording a new manifest version.
