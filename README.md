# LeanToken

LeanToken is a token-bounded repository retrieval kernel for coding agents.
One process serves one repository and one atomically published, immutable index
generation.

```text
refresh -> complete bounded generation -> atomic publish
                                      -> search
                                      -> outline
                                      -> read
                                      -> context
```

Repository edits are deliberately invisible until `refresh` succeeds. Queries
never reopen live source files, and a cancelled refresh leaves the previous
generation available. Start a separate process for a separate repository.

## CLI

```bash
cargo run -- refresh
cargo run -- search needle
cargo run -- outline src/lib.rs
cargo run -- read src/lib.rs --lines 1:80
cargo run -- context --task "find cancellation ownership"
cargo run -- mcp
```

All retrieval commands use the latest published generation. Use native tools
for edits, builds, tests, Git history, and intentionally live dirty-file reads.

## MCP

The rmcp 3.1.2 server exposes exactly five tools: `refresh`, `search`,
`outline`, `read`, and `context`. Search, outline, and read pagination use one
authenticated cursor envelope bound to the server process, repository,
generation, and normalized request.

See [architecture](docs/architecture.md) for invariants and bounds and
[development](docs/development.md) for validation commands.

## License

MIT OR Apache-2.0.
