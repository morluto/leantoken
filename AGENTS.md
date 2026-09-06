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

Local tests use disposable fixtures and have no production access. Run relevant
checks, fix change-caused failures, and rerun affected tests without asking for
approval. Before the first push, format the tree and run a behavioral check that
proves the change; the complete CI suite need not block opening a pull request.
Use `cargo test-focused <module-or-test>` to filter the library, binary, and
integration targets, and use the ownership map in `docs/development.md` when
choosing affected tests.

GitHub Actions owns normal Rust merge readiness and must pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test-product
```

It also runs `cargo test-extras` for examples and benchmark behavior and checks
rustdoc for public APIs and documentation. Run a complete gate locally only
when reproducing CI, working without CI, or changing the gate itself. See
`docs/development.md` for the complete development, packaging, and release
workflow.

## Change-specific validation

- For performance investigations, `$optimize-accuracy-first` points to the
  repository's measurement tools. Ordinary retrieval fixes do not need a
  performance study or paired agent evaluation.
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
