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

## Test responsibilities

Tests are organized around observable behavior:

- `tests/storage.rs`: migrations, WAL/foreign keys, FTS5, atomic replacement,
  rollback, reopen, and generation behavior;
- `tests/indexer.rs`: initial, unchanged, changed, deleted, rebuilt, bounded
  chunking, targeted reconciliation, and dependency invalidation;
- `tests/services.rs`: all five retrieval services, token bounds, ranges,
  hashes, typed invalid inputs, and continued service after rejection;
- `tests/mcp.rs`: SDK initialization, exact tool catalog, structured calls,
  cancellation, and post-cancellation liveness;
- `tests/binary.rs`: CLI JSON flow and MCP EOF shutdown through the executable;
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
cargo test --test benchmark_contract -- --nocapture
```

The representative benchmark requires pinned external worktrees and `rg`. See
[`../benchmarks/README.md`](../benchmarks/README.md) for preparation, command
line, measurements, and interpretation limits.

The same guide documents the synthetic indexing and file-read profile used to
gate targeted reconciliation and any future hot-file cache.

Keep negative results. Do not tune prompts, labels, or budgets after seeing a
result without recording a new manifest version.
