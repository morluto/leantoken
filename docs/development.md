# Development and testing

## Prerequisites

LeanToken requires Rust 1.95 or later and a native C/C++ toolchain for bundled
SQLite and tree-sitter grammar crates. On macOS, install Xcode Command Line
Tools. On Windows, install Visual Studio Build Tools.

## Local checks

Run the same checks used by CI:

```bash
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo test --all-targets --all-features
```

Build and verify the distributable crate when changing packaging or features:

```bash
cargo build --release
cargo package
```

## Release artifacts

The generated release workflow builds native archives for Linux x64/arm64,
macOS x64/arm64, and Windows x64. A custom packaging job converts those
archives into one `leantoken` npm package containing all five native binaries.
The included JavaScript launcher selects the binary for the current OS and CPU;
npm installation does not run lifecycle scripts or download a binary from a
postinstall hook.

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

npm publication is currently manual. Once the GitHub release finishes, inspect
the package before publishing it:

```bash
tar -xOf leantoken-VERSION.tgz package/package.json
npm publish leantoken-VERSION.tgz --dry-run
```

The dry-run file list must contain one binary for every target in
`npm/platforms.json`, and the manifest must not define lifecycle scripts or
dependencies. Publish only after those checks pass:

```bash
npm publish leantoken-VERSION.tgz --access public
```

Confirm the release from the registry rather than a local package or npm cache:

```bash
npm view leantoken@VERSION version
npx --yes leantoken@VERSION --version
```

Configure trusted publishing for `leantoken` before automating later npm
releases.

## Test responsibilities

Tests are organized around observable behavior:

Integration test files are modules of one `integration` target so Cargo can
run them in parallel rather than starting one executable per file.

- `tests/storage.rs`: migrations, WAL/foreign keys, FTS5, atomic replacement,
  rollback, stale-plan rejection, reopen, and generation behavior;
- `tests/indexer.rs`: initial, unchanged, changed, deleted, rebuilt, bounded
  chunking, targeted reconciliation, and dependency invalidation;
- `tests/services.rs`: all five retrieval services, token bounds, ranges,
  hashes, cache-artifact exclusion, typed invalid inputs, retryable generation
  conflicts, and continued service after rejection;
- `tests/mcp.rs`: SDK initialization, readiness states, retryable startup tool
  errors, exact tool catalog, structured calls, cancellation, and
  post-cancellation liveness;
- `tests/binary.rs`: CLI JSON flow, concurrent and contended cold-cache MCP
  initialization, runtime-failure visibility, single-leader generation
  publication, leader failover, and MCP EOF shutdown through the executable;
- `tests/repository.rs`: ignore behavior, path validation, size limits, symlink
  containment, bounded Git probes, and nested-worktree path normalization;
- `tests/watcher.rs`: event delivery and joined shutdown;
- `tests/benchmark_contract.rs`: token-economy and known-hash regression fixture;
- `tests/mcp_token_costs.rs`: real tool catalog and JSON-RPC handoff accounting;
- `tests/representation_comparison.rs`: tree, outline, search, read, and context
  representation costs.

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

Prefer query-plan evidence (`EXPLAIN QUERY PLAN`) and focused integration tests
for storage changes. A faster microbenchmark is insufficient if it weakens
atomic publication, stale-plan rejection, bounded memory, or deterministic
results.

## MCP schema snapshots

The generated five-tool catalog is snapshot-tested. Review snapshot changes as
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
reporting. This is an acknowledged source-compatibility change for consumers
that exhaustively matched the earlier enum. Release PRs own package version
changes; feature and fix PRs do not edit `Cargo.toml` versions independently.

`IndexResponse` retains its original constructible field set for downstream
Rust source compatibility. Additive preparation accounting is exposed through
`IndexReport`, returned by the new `Indexer::*_report` and
`Services::*_report` methods. The report flattens the compatible response for
JSON output, so CLI consumers receive `skip_reasons` without forcing existing
Rust consumers to update struct literals or destructuring patterns.

Use `InvalidRequest` only for audited caller validation. Infrastructure and
invariant failures use `InternalFailure`, which retains the historical
`invalid request: ...` display prefix for CLI text compatibility while adapters
classify it as internal. Do not infer error categories from rendered strings.

## Benchmarks

The fixture benchmark is a fast regression check:

```bash
cargo test --test integration benchmark_contract:: -- --nocapture
```

The representative benchmark requires pinned external worktrees and `rg`. See
[`../benchmarks/README.md`](../benchmarks/README.md) for preparation, command
line, measurements, and interpretation limits.

The frozen validation set, ablation runner, isolated model A/B adapter, and
exact MCP wire proxy are documented in [`measurement.md`](measurement.md).

The same guide documents the synthetic indexing and file-read profile used to
gate targeted reconciliation and any future hot-file cache.

Keep negative results. Do not tune prompts, labels, or budgets after seeing a
result without recording a new manifest version.
