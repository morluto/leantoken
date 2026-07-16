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
macOS x64/arm64, and Windows x64. It also builds the `leantoken` npm installer,
which selects and downloads the matching archive from the GitHub release.

Verify release configuration changes before pushing them:

```bash
dist generate --check
dist plan
```

Version tags such as `v0.1.0` trigger `.github/workflows/release.yml`. Keep the
Cargo package version, tag, GitHub release, and npm package version identical.

The first npm release is a manual bootstrap because npm trusted publishing can
only be configured after the package exists. Once the GitHub release finishes,
download and extract `leantoken-npm-package.tar.gz`, then publish its `package`
directory with `npm publish package --access public`. Configure trusted
publishing before automating later npm releases.

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
