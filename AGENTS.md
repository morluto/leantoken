# Repository guidance

LeanToken is a Rust application and library providing token-bounded repository
retrieval through CLI and MCP adapters.

## Architecture

- `Services` owns application behavior; CLI and MCP adapters remain thin.
- Preserve deterministic ranking, exact token budgets, bounded memory, atomic
  SQLite generations, and request snapshot consistency.
- Document new scan, fan-out, storage, and concurrency bounds in
  `docs/architecture.md`.

## Development

Run focused tests while iterating. For normal Rust pull-request readiness:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test-product
```

Run `cargo test-extras` when changing examples or benchmark behavior. Run
rustdoc checks when changing public APIs or documentation. See
`docs/development.md` for the complete development, packaging, and release
workflow.

## Change-specific validation

- For performance or scalability work, use `$optimize-accuracy-first`.
- For storage changes, include query-plan evidence and focused integration
  tests; a faster microbenchmark does not justify weaker atomicity, limits,
  freshness, or deterministic results.
- Treat MCP schema snapshots as protocol changes. Inspect the schema diff before
  accepting an `insta` update.
- Prefer behavioral integration tests for observable contracts and unit tests
  for private invariants.
- Match surrounding Rust naming, documentation, and comment conventions.

## Contributions

Use conventional commit prefixes and follow `.github/pull_request_template.md`.
For releases and npm publication, follow `docs/development.md` and
`docs/releases.md`; never replace an already-published version.
