# LeanToken — token-bounded repository context for coding agents

LeanToken gives coding agents the relevant parts of a repository without
repeatedly reading whole files. It runs locally as a read-only CLI and MCP
server, indexes source in SQLite, and keeps retrieval within explicit token
budgets.

## Installation

LeanToken requires Rust 1.95 or later and a native C/C++ toolchain for bundled
SQLite and the tree-sitter grammars.

```bash
cargo install --git https://github.com/morluto/leantoken
```

Set up LeanToken for your coding agents with one command:

```bash
leantoken setup
```

The wizard detects installed clients, lets you choose which ones to configure,
and registers the absolute LeanToken executable as a global MCP server. Each
client launches it in the active workspace.

Supported clients are Claude Code, Cursor, OpenCode, Codex, Gemini CLI, and
Antigravity. To skip the wizard, select clients explicitly or configure all of
them:

```bash
leantoken setup --claude --codex --yes
leantoken setup --all --yes
```

Setup installs only the `leantoken` MCP entry. It does not add skills, rules,
or shell hooks. Remove the generated entries later with:

```bash
leantoken remove
leantoken remove --all --yes
```

## What changes

Without LeanToken, repository exploration often means broad file searches,
whole-file reads, and repeated source after a handoff. With LeanToken, the agent
can start with ranked task context, inspect structure, and retrieve only the
source ranges it needs.

The host agent still owns editing, commands, tests, conversation state, and
model orchestration. LeanToken handles repository discovery and bounded source
retrieval.

## Available tools

| Tool | Purpose |
| --- | --- |
| `leantoken_context` | Start a task with ranked excerpts selected within a token budget. |
| `leantoken_files` | Browse a compact tree or find paths without source bodies. |
| `leantoken_search` | Search text, regex, identifiers, symbols, or syntactic references. |
| `leantoken_outline` | Return definitions, signatures, parents, imports, and ranges. |
| `leantoken_read` | Read an exact line or symbol range under a token limit. |

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

## Current benchmark signal

The checked-in representative run found 13 of 15 labeled files while returning
4,640 source tokens, compared with 143,034 tokens for full contents of the
labeled files. The complete LeanToken responses used 12,902 tokens. It covered
only 10 of 41 labeled line anchors, so this is evidence of a smaller payload—not
proof that the excerpts are sufficient to implement the fixes. See the
benchmark documentation for the retrospective corpus, methodology, and
limitations.

## License

MIT OR Apache-2.0
