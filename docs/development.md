# Development

## Prerequisites

LeanToken requires Rust 1.95 or newer and a native C/C++ toolchain for bundled
SQLite and tree-sitter grammars. Install `cargo-nextest` for the complete local
product suite and `cargo-insta` before reviewing snapshot changes.

```bash
cargo install cargo-nextest --locked
cargo install cargo-insta --locked
python -m pip install pre-commit
pre-commit install --install-hooks
```

## Working loop

Run formatting and the smallest behavioral owner while iterating:

```bash
cargo fmt --all -- --check
cargo test-focused storage
cargo test-focused services::read
```

`cargo test-focused` requires one domain or test selector and fails on zero or
ambiguous matches. Use direct Cargo targets when the owner is already known:

```bash
cargo test --locked --package leantoken --all-features --lib FILTER
cargo test --locked --package leantoken-test-suite --all-features \
  --lib domains::DOMAIN::FILTER
cargo test --locked --package leantoken --all-features \
  --test integration process::FILTER -- --test-threads=2
```

Inspect the complete product plan with:

```bash
cargo xtask test plan --dry-run
cargo xtask check-test-architecture
```

The normal merge gates are owned by GitHub Actions:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test-product
```

Run `cargo test-extras` when benchmark executables or their fixtures change,
and `cargo test-contract` when token-economy accounting changes. Run the full
local gate only to reproduce CI, change the gate, or work without CI.

## Test ownership

Tests follow behavior, not file size or implementation layers:

- Product unit tests own private parsing, ranking, tokenization, watcher, and
  service invariants.
- `crates/test-suite/src/domains/storage.rs` owns migrations, query plans,
  atomic publication, reopen, rejection, and generation behavior.
- `domains/indexing_repository.rs` owns discovery, path safety, refresh,
  rebuild, deletion, and dependency invalidation.
- `domains/retrieval.rs` owns public search, outline, read, budget, cursor,
  known-hash, and artifact composition contracts.
- `domains/platform.rs` owns configuration, cache identity, native watcher
  delivery, and shutdown.
- `domains/protocol.rs` and `domains/contracts.rs` own RMCP lifecycle, tool
  schemas, JSON-RPC composition, cancellation, and liveness.
- `tests/services/` owns service workflows and cross-operation contracts.
- `tests/process/` owns real executable behavior, multiprocess publication,
  failover, and MCP EOF shutdown.
- `crates/git/src/tests.rs` owns bounded Git parsing and subprocess behavior.
- `crates/lab/src/lib.rs` owns offline artifact analyzers.

Keep the state-machine tests for refresh/query/cancel/page/restart
interleavings, but do not use them as a substitute for search, outline, read,
path-safety, watcher-recovery, MCP-lifecycle, and process-isolation evidence.

## Architecture changes

Storage and retrieval changes must preserve:

- one pinned `RepositoryGeneration` per multi-query response;
- atomic replacement and stale-plan rejection;
- deterministic ranking and pagination;
- exact source and serialized-response budgets;
- bounded scans, fan-out, memory, workers, connections, and subprocess output;
- generation-backed canonical reads; and
- failure isolation between generation, artifact, and instrumentation storage.

Use versioned migrations and test both new and upgraded databases. Add query
plan evidence for changed SQLite access paths. Record new scan, fan-out,
storage, or concurrency bounds in [architecture.md](architecture.md).

Candidate generation, ranking, allocation, or default-signal changes require
the [retrieval promotion gate](../benchmarks/README.md#retrieval-promotion-gate)
against the same frozen manifest. A benchmark win cannot trade away atomicity,
freshness, deterministic results, or bounded behavior.

## MCP and public APIs

Treat MCP schema snapshots as protocol changes. Inspect tool names,
descriptions, required fields, defaults, and total catalog size before accepting
an `insta` update:

```bash
cargo insta review
```

RMCP is pinned exactly in `Cargo.toml`; update it only with focused protocol
tests and package validation. CLI and MCP adapters remain thin projections over
`Services`.

The Rust crate is on a `0.x` development line. Public errors are non-exhaustive;
consumers need a fallback match arm. Release PRs own version changes. Do not
encode error categories by parsing rendered strings.

## Pull requests

Before the first push, format and run the smallest relevant behavioral proof.
Use the repository pull request template, keep each commit reviewable, and list
only tests that actually ran. For storage, concurrency, protocol, or other
cross-cutting changes, review the exact branch diff and obtain one independent
consolidated review before merge.

Documentation describes the current product contract. Planning belongs in
issues and pull requests; dated experiment narratives belong in Git history or
machine-readable evidence, not evergreen guides.

## Packaging and releases

Run rustdoc for public API changes and package checks for distribution changes:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo build --release
cargo package
```

The npm package contains the native binaries described by
`npm/platforms.json` and has no install-time downloader. Validate packaging with
the existing Node tests when those files change. Publication and recovery are
documented in [releases.md](releases.md).
