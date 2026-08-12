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

Install `cargo-nextest` when running the complete local product suite; CI
installs the repository's nextest release automatically:

```bash
cargo install cargo-nextest --locked
```

Install `cargo-insta` before reviewing intentional snapshot updates:

```bash
cargo install cargo-insta --locked
```

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

Run a focused test module while developing:

```bash
cargo test-focused services::
cargo test-focused platform
```

Named suite domains (`indexing_repository`, `storage`, `retrieval`, `protocol`,
`platform`, and `contracts`) route directly to their owning package. Other
module or exact-test filters search both the product and domain-suite packages.
Zero matches fail instead of returning false-green; a name present in both
packages fails as ambiguous and asks for a domain-qualified selector. Use the
ownership map under [Test responsibilities](#test-responsibilities), and run
each affected filter when a change crosses boundaries.

When the exact owner is already known, use its target directly to avoid
building unrelated harnesses:

```bash
# Private product invariant
cargo test --locked --package leantoken --all-features --lib FILTER

# Cross-component domain contract
cargo test --locked --package leantoken-test-suite --all-features \
  --lib domains::DOMAIN::FILTER

# Real executable or MCP process behavior
cargo test --locked --package leantoken --all-features \
  --test integration process::FILTER -- --test-threads=2

# CLI binary unit or one example harness
cargo test --locked --package leantoken --all-features --bin leantoken FILTER
cargo test --locked --package leantoken-benchmarks --bin NAME FILTER
```

Run the complete product-behavior suite without compiling or executing the
benchmark contract or examples:

```bash
cargo test-product
```

For a visible, resource-safe phase plan use the Rust workspace task runner:

```bash
cargo xtask test plan --dry-run
cargo xtask check-test-architecture
```

The source tree uses ordinary Rust modules for organization. The architecture
check rejects any organizational `include!()` directive so the old namespace
concatenation pattern cannot return.

The runner sequences units, ordinary domain integration, and process-heavy
tests. CI uses `cargo xtask test product --parallel` to overlap only the
library/binary unit lane and ordinary integration lane; those phases use
`cargo-nextest` with two workers each (a maximum of four across the overlap),
while process-heavy executable/MCP behavior uses three nextest workers on macOS,
four on Linux, and two on Windows. Windows process tests can each launch
several child processes, so the lower bound avoids starving those children.
Doctests stay on their explicit Cargo command.
The runner prints per-lane elapsed time and preserves child exit codes. It also
owns the opt-in profile and stress commands. Checked-in corpora and benchmark
reports execute through their explicit domain, contract, or example owner
rather than a generic serialized-case phase.

The stress lane accepts `LEANTOKEN_STRESS_REPETITIONS` for scheduled
repetition. Required checks never retry failures.

The weekly `cargo xtask test profile` lane uses the checked-in nextest policy:
tests slower than ten seconds are reported, a deadlocked test is terminated
after a bounded interval, and retries are disabled.

Run the token-economy contract explicitly when changing retrieval accounting or
its fixture. CI selects this lane for its owned source, suite, fixture, and
manifest paths on every supported OS:

```bash
cargo test-contract
```

This full local command uses a platform-aware process-test bound: three workers
on macOS, four on Linux, and two on Windows. That keeps child-process load
bounded while using the standard runner capacity. Focused tests retain Cargo's
normal host parallelism.

Benchmark and example tests are a separate target group because Cargo executes
test binaries serially. Run them when changing `examples/`, benchmark fixtures,
or their shared behavior:

```bash
cargo test-extras
cargo test --locked --package leantoken --all-features --doc
```

When changing TypeScript grammar integration, extraction on incomplete trees,
or the manual recovery evaluator, run its focused synthetic contract:

```bash
cargo test --locked --package leantoken-benchmarks --bin typescript_parse_diagnostic
cargo run --locked --release --package leantoken-benchmarks --bin typescript_parse_diagnostic -- \
  verify-fixture
```

The pinned external-corpus command, immutable output contract, and
interpretation limits are documented in
[`../benchmarks/README.md`](../benchmarks/README.md#typescript-parse-recovery-diagnostic).
The external run is evidence for parser work, not a normal local or CI gate.

When changing the Swift evaluation manifest, diagnostic, reports, or its
development-only grammar pin, run:

```bash
CARGO_TARGET_DIR=target cargo test --locked \
  --manifest-path benchmarks/swift-grammar-073/Cargo.toml \
  --bin swift-parse-diagnostic-073
```

The exact external-corpus command, frozen retrieval gate, and no-ship result
are documented in
[`../benchmarks/README.md`](../benchmarks/README.md#swift-structural-indexing-evaluation).
The excluded manifest owns the runnable 0.7.3 diagnostic and its independent
lockfile. Swift remains unsupported by production structural parsing; this
target is an evaluation contract and must not be treated as an index-readiness
check.

When changing the Kotlin evaluation manifest, diagnostic, reports, or its
research-only grammar pin, run:

```bash
CARGO_TARGET_DIR=target cargo test --locked \
  --manifest-path benchmarks/kotlin-grammar-040/Cargo.toml
```

The exact external-corpus command, frozen retrieval gate, and no-ship result
are documented in
[`../benchmarks/README.md`](../benchmarks/README.md#kotlin-structural-indexing-evaluation).
The excluded manifest pins the exact unreleased 0.4.0 grammar commit without
adding it to the normal dependency graph. Its corpus diagnostic reads blobs
from the requested commit rather than from the checkout, reports `.kt` and
`.kts` extension aggregates separately, and labels grammar syntax-node counts
separately from production extraction. Kotlin remains unsupported by
production structural parsing; this target is an evaluation contract, not an
index-readiness check.

When changing the Python resolved-reference oracle, its frozen labels, or its
checked reports, run:

```bash
cargo test --locked --package leantoken-benchmarks \
  --bin resolved_reference_oracle
cargo run --locked --release --package leantoken-benchmarks \
  --bin resolved_reference_oracle -- verify-fixture
```

The fixture, exact comparison, resource receipt, coverage gaps, and no-public-tool
decision are documented in
[`../benchmarks/README.md`](../benchmarks/README.md#python-resolved-reference-oracle).
This is an evaluation contract; it does not establish production binding
semantics or authorize a CLI or MCP surface.

These repository-local Cargo aliases keep the fast and extended target groups
consistent with CI. The development profile retains line tables for useful
backtraces in LeanToken while omitting dependency debug information, which
keeps links and the local `target` directory smaller. Override it temporarily
with `CARGO_PROFILE_DEV_DEBUG=2` when a debugger needs full local-variable data
for LeanToken code. When debugging a dependency too, pass
`--config 'profile.dev.package."*".debug=2'` to Cargo.

Inspect accumulated build artifacts without deleting them:

```bash
python3 scripts/report_target_footprint.py
python3 scripts/report_target_footprint.py --json
```

The report separates incremental, dependency, example, build-script, release,
and other artifacts; it also counts incremental generations inactive beyond
the selected `--stale-days` threshold. The scan does not follow symlinks, fails
closed after one million entries by default, and never removes files. If a
reviewed report justifies paying the rebuild cost, `cargo clean --profile dev`
is the explicit Cargo-owned cleanup for the development profile. Hooks and CI
must not clean a developer's target directory automatically.

## Pull request readiness

Before the first push, format the tree and run the smallest relevant check or
behavioral test that proves the change. Open the pull request once that focused
proof passes so the broad checks can run in parallel with review.

GitHub Actions is the merge gate for:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test-product
cargo test-contract
```

The CI planner selects product, token-economy contract, benchmark/example, and
coverage lanes independently from their owned paths. Selected benchmark,
example, and documentation tests run once on Linux. Selected product and
contract lanes run on Linux, macOS, and Windows with per-lane elapsed summaries
from xtask. The process-heavy phase uses three workers on macOS, four on Linux,
and two on Windows because each test can start several child processes;
ordinary tests retain the runner's default parallelism. Selected Rust changes
also run the instrumented coverage gate in parallel (50% line floor; the opt-in
`concurrency_profile` harness and subprocess-only CLI entrypoints are
excluded). The stable Required checks job fails if a selected lane fails,
cancels, times out, or disappears, while intentionally unselected lanes remain
conditional.
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
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
```

Build and verify the distributable crate when changing packaging or features:

```bash
cargo build --release
cargo package
```

`rmcp` is an upstream registry dependency pinned to the exact version in
`Cargo.toml`; `Cargo.lock` records the resolved checksum. Update it only with the
protocol tests and package validation above. `cargo package` validates the
distributable crate, while publication follows [Release process](releases.md)
and never replaces an already-published version.

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

CI runs the host-native installation test on Linux, macOS, and Windows when the
planner selects the npm lane.

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

- `crates/test-suite/src/domains/storage.rs`: migrations, WAL/foreign keys,
  FTS5, atomic replacement, rollback, stale-plan rejection, reopen, query-plan
  evidence, and generation behavior;
- `crates/test-suite/src/domains/indexing_repository.rs`: discovery and path
  safety, Git diffs, initial/unchanged/changed/deleted/rebuilt indexing,
  bounded chunking, targeted reconciliation, and dependency invalidation;
- `crates/test-suite/src/domains/retrieval.rs`: public retrieval primitive
  compatibility plus cross-component budget, scope, known-hash omission, and
  receipt composition; detailed tokenizer and ranking behavior stays with the
  owning production modules;
- `crates/test-suite/src/domains/platform.rs`: public configuration path,
  cache identity, safety, and limit boundaries plus native watcher delivery
  and shutdown;
- `crates/test-suite/src/domains/protocol.rs`: SDK initialization, readiness
  states, retryable startup errors, tool calls, cancellation, and liveness;
- `crates/test-suite/src/domains/contracts.rs`: real tool-catalog and JSON-RPC
  handoff accounting;
- `tests/services.rs` registers owner-focused modules under `tests/services/`:
  lifecycle/repository/consistency/path safety, per-tool behavior, limits and
  response budgets, context planning/signals/workflows/diffs, evidence/query
  receipts and savings, language coverage, and smoke behavior;
- `tests/process.rs`: CLI JSON flow, concurrent and contended cold-cache MCP
  initialization, runtime-failure visibility, single-leader generation
  publication, leader failover, MCP EOF shutdown, and repository-free episode
  audit behavior through the executable;
- `tests/benchmark_contract.rs`: explicit token-economy and known-hash regression executable;
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

The generated nine-tool catalog is snapshot-tested. Review snapshot changes as
protocol changes: tool names, descriptions, required fields, defaults, and
schema size all consume client context or affect compatibility.

Update an intentional snapshot with:

```bash
cargo insta review
```

Do not accept a snapshot solely because generation changed; inspect the schema
diff first.

## Public Rust API and wire boundaries

The crate remains on the `0.x` development line. `Error` is intentionally
non-exhaustive: consumers must include a fallback arm and should only branch on
variants they can recover from. LT-06 establishes that contract while adding
`RequestLimitExceeded`, whose fields are required for adapter-safe limit
reporting. `ResponseBudgetExceeded` separately represents a computed,
exactly-retryable response minimum and exposes only bounded aggregate
accounting. These are acknowledged source-compatibility changes for consumers
that exhaustively matched the earlier enum. Release PRs own package version
changes; feature and fix PRs do not edit `Cargo.toml` versions independently.

`IndexResponse` remains the stable response core. Additive preparation
accounting is exposed through `IndexReport`, returned by the new
`Indexer::*_report` and `Services::*_report` methods. The report keeps those
core fields flattened in JSON, so CLI consumers receive `skip_reasons` without
changing the established response shape.

`CacheListRequest` and `CachePruneRequest` are the canonical cache operation
inputs. The cache-list response retains its `CacheListReport` JSON shape and
`cl2` cursor encoding because those are cache-format boundaries; the CLI and
internal cache manager use the same request path for metadata and content
compatibility filters.

Explicit indexing scope adds public provenance fields to `ResponseMeta`,
`StatusResponse`, and `CacheEntry`, plus the `IndexScopeMismatch` error variant.
This is wire-additive and older deserialization defaults to full scope, but
downstream Rust consumers constructing those public response structs with
literals must add the new fields. `IndexScope` is immutable after
normalization; use `Config::discover_scoped` instead of mutating cache
membership after service startup.

Use `InvalidRequest` only for audited caller validation. Serialization,
response-accounting, cache-pruning, setup, and operation failures each have a
dedicated typed variant and machine-readable category. Do not infer error
categories from rendered strings.

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
contract test remains in the benchmark package.

On Linux, reproduce the stdio MCP ownership and resource profile after building
the product binary in release mode:

```bash
cargo build --release
cargo run --release -p leantoken-benchmarks --bin mcp_multiprocess_profile -- \
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
cargo run --release -p leantoken-benchmarks --bin semantic_change_receipt_benchmark -- \
  --iterations 21 \
  --output target/semantic-change-receipt-v1.json
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
