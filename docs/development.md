# Development

LeanToken requires Rust 1.95 and a native C/C++ toolchain for bundled SQLite
and tree-sitter grammars.

Use focused tests while changing one semantic owner:

```bash
cargo test-focused generation_state_machine
cargo test-focused mcp::tests
cargo test-focused storage::tests
```

Before handoff, run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test-product
```

Test ownership is intentionally small: private unit invariants live beside
their module; cross-component generation behavior lives in
`tests/services/generation_state_machine.rs`; rmcp composition lives in the MCP
unit module; SQLite publication and query plans remain storage tests. There is
no benchmark workspace, test scheduler, protocol mega-test, watcher stress
lane, or receipt/delta suite.

New work must preserve deterministic ranking, exact token accounting, bounded
memory and scan work, atomic publication, and request snapshot consistency.
Document any new scan, fan-out, storage, or concurrency bound in
`docs/architecture.md`.
