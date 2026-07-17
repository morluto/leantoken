# Repository Guidelines

## Project Structure

```
src/           Application crate (lib + bin)
  cli.rs         CLI argument parsing and AppRequest dispatch
  config.rs      Repository config and cache-path discovery
  error.rs       Shared error and result types
  indexer.rs     File discovery, parsing, and reconciliation
  main.rs        Binary entrypoint, tracing init, watcher lifecycle
  mcp.rs         MCP stdio server adapter
  model.rs       Request/response models shared by CLI, MCP, services
  parser.rs      Tree-sitter language detection and syntax extraction
  ranking.rs     Deterministic evidence ranking and deduplication
  repository.rs  Ignore-aware file discovery
  services.rs    Token-bounded repository retrieval services
  storage.rs     SQLite schema, FTS5, transactions
  text.rs        UTF-8 chunking, hashing, line-range helpers
  tokens.rs      tiktoken-based token counting and truncation
  watcher.rs     Debounced filesystem watcher and reconciliation
tests/          Integration and behavioral tests
benchmarks/     Token-economy benchmarks
docs/           Usage, architecture, roadmap, development guides
```

All application logic lives in `Services`; adapters (CLI, MCP) are thin layers
over the same service methods.

## Build, Test, and Development Commands

```bash
cargo build --release                  # Release build (LTO thin, stripped)
cargo check --all-targets --all-features  # Fast compilation check
cargo fmt --all -- --check             # Format check
cargo clippy --all-targets --all-features -- -D warnings  # Lint with deny
cargo test --all-targets --all-features   # Full test suite
cargo doc --no-deps                    # Doc generation
```

Snapshot tests use `insta`. Review intentional changes with `cargo insta review`.

## Expensive diagnostics

- Run benchmarks and repository-scale profilers with `--release`; debug-mode
  timings are not decision evidence.
- If a diagnostic is unexpectedly expensive, inspect CPU activity, build mode,
  and the active corpus promptly. Do not wait through a long run merely because
  it is still making progress.
- Stop and correct avoidable harness or build-mode cost once identified, then
  rerun the smallest representative measurement.
- When the cost is product behavior, capture a focused reproduction and address
  the owner boundary immediately when the fix is in scope. Otherwise open a
  focused issue with measured evidence, correctness constraints, and success
  criteria before continuing broad evaluation.

## Coding Style & Naming Conventions

- Rust edition 2024. Minimum supported Rust version is 1.95.
- `unsafe_code = "forbid"`. All clippy lints at warn level.
- Module names use `snake_case`. Types use `CamelCase`.
- Public API is in `src/lib.rs`; the binary re-exports nothing unused.
- Errors use `thiserror` derive macros. Prefer structured variants over string
  messages.
- Tracing goes to stderr via `tracing-subscriber`; structured output to stdout
  as JSON via `serde_json`.
- Code comments above declarations describe intent, not mechanics. Doc comments
  (`//!` and `///`) are required on all public items.

## Testing Guidelines

- **Integration tests** in `tests/` cover observable behavior across storage,
  indexing, services, MCP, binary, repository, watcher, and benchmark contracts.
- **Unit tests** live inline next to the owning module where private invariants
  matter (parser, text, ranking, tokens).
- Test names describe the scenario: `test_<behavior>_<condition>`.
- CI runs the full suite on Linux, macOS, and Windows. Platform-specific
  behavior (watchers, paths) must pass the matrix before merging.

## Commit & Pull Request Guidelines

- Use conventional commits: `feat:`, `fix:`, `docs:`, `bench:`, `ci:`, `chore:`.
- Branches follow `feat/<description>` or `fix/<description>`.
- PRs should include a brief description of what changed and why. Link related
  issues if applicable.
- MCP schema snapshot changes require explicit review of the diff before
  accepting.

## npm Publication

- Keep the Cargo package version, Git tag, GitHub release, and npm package
  version identical.
- Publish only the root `leantoken` package. Its tarball must contain every
  native target listed in `npm/platforms.json`; do not publish a partial bundle.
- Follow the verification and publication commands in `docs/development.md`.
  Never attempt to replace an npm version that has already been published; cut
  the next release instead.
