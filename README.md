# LeanToken — token-bounded repository context for coding agents

LeanToken gives coding agents the relevant parts of a repository without
repeatedly reading whole files. It runs locally as a read-only CLI and MCP
server, indexes source in SQLite, and keeps retrieval within explicit token
budgets.

## Installation

Configure LeanToken for your coding agents:

```bash
npx leantoken setup
```

The wizard finds supported clients and registers LeanToken as a global MCP
server. Each client launches the current release in its active workspace.

Supported clients are Claude Code, Cursor, OpenCode, Codex, Gemini CLI, and
Antigravity. To skip the wizard, select clients explicitly or configure all of
them:

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

Setup adds only the `leantoken` MCP entry. It does not add skills, rules, or
shell hooks. Remove it with:

```bash
npx leantoken remove
```

To use the CLI directly, run commands through npx:

```bash
npx leantoken status
npx leantoken --root /path/to/repo search handle_request
```

Or install the CLI globally to omit the npx prefix:

```bash
npm install --global leantoken
```

To build from source, install Rust 1.95 or later and a native C/C++ toolchain:

```bash
cargo install --git https://github.com/morluto/leantoken
```

## Updating

MCP entries created by setup follow current npm releases automatically. No
manual MCP update is required.

For a globally installed CLI:

```bash
leantoken upgrade
```

Use `--check` to check without installing or `--yes` to skip confirmation.
`update` is an alias for `upgrade`.

```bash
leantoken upgrade --check
leantoken upgrade --yes
leantoken update
```

## What changes

Without LeanToken, repository exploration often means broad file searches,
whole-file reads, and repeated source after a handoff. With LeanToken, the agent
can discover paths, inspect structure or ranked matches, and retrieve only the
source ranges it needs. Ranked task context remains available when the scope is
still uncertain.

The host agent still owns editing, commands, tests, conversation state, and
model orchestration. LeanToken handles repository discovery and bounded source
retrieval.

## Available tools

| Tool | Purpose |
| --- | --- |
| `leantoken_files` | Browse a compact tree or find paths without source bodies. |
| `leantoken_search` | Search text, regex, identifiers, symbols, or syntactic references. |
| `leantoken_outline` | Return definitions, signatures, parents, imports, and ranges. |
| `leantoken_read` | Read an exact line or symbol range under a token limit. |
| `leantoken_context` | Rank task evidence when narrow discovery leaves the scope uncertain. |

The catalog stays small because every tool description and schema consumes
model context.

## CLI usage

LeanToken can also be used directly:

```bash
leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

Run the MCP server manually over stdio:

```bash
leantoken --root /path/to/repo mcp
```

Example manual client configuration:

```json
{
  "mcpServers": {
    "leantoken": {
      "command": "leantoken",
      "args": ["--root", "/path/to/repo", "mcp"]
    }
  }
}
```

## How it works

LeanToken indexes source once, then serves compact paths, ranked matches,
structural outlines, exact source ranges, and task-specific context. Progressive
disclosure and content hashes reduce broad reads and suppress unchanged evidence
across turns or model handoffs.

The primary metric is useful repository evidence delivered per model token.

## Documentation

- [Usage and tool reference](docs/usage.md)
- [Architecture and reliability](docs/architecture.md)
- [Roadmap](docs/roadmap.md)
- [Development and testing](docs/development.md)
- [Benchmark methodology](benchmarks/README.md)
- [Experiment and wire-cost harnesses](docs/measurement.md)

## License

MIT OR Apache-2.0
