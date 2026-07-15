# LeanToken

LeanToken gives coding agents a small, relevant slice of a repository instead
of making them repeatedly read whole files. It is a local, read-only CLI and
MCP server built around explicit token budgets.

It indexes source into SQLite once, then serves compact paths, ranked matches,
structural outlines, exact source ranges, and task-specific context. The host
agent still owns editing, commands, tests, conversation state, and model
orchestration.

## Why

Repository exploration is often the largest part of an agent's context. A
plan can summarize what was learned, but it cannot replace the source evidence
needed to make an edit. LeanToken cuts repeated reading through progressive
disclosure, bounded output, relevance ranking, and content hashes that suppress
unchanged evidence across turns or model handoffs.

The primary metric is useful repository evidence delivered per model token.

## Tools

| Tool | Purpose |
| --- | --- |
| `leantoken_files` | Browse a compact tree or find paths without source bodies. |
| `leantoken_search` | Search text, regex, identifiers, symbols, or syntactic references. |
| `leantoken_outline` | Return definitions, signatures, parents, imports, and ranges. |
| `leantoken_read` | Read an exact line or symbol range under a token limit. |
| `leantoken_context` | Select and explain task-relevant excerpts within a token budget. |

The catalog stays small because every tool description and schema consumes
model context.

## Install

LeanToken requires Rust 1.95 or later and a native C/C++ toolchain for bundled
SQLite and the tree-sitter grammars.

```bash
git clone https://github.com/morluto/leantoken.git
cd leantoken
cargo install --path .
```

## Quick start

```bash
leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

Run the MCP server over stdio:

```bash
leantoken --root /path/to/repo mcp
```

Or register LeanToken globally with supported coding clients. The interactive
wizard detects installed clients and stores the absolute LeanToken executable;
each client launches it in the active workspace.

```bash
leantoken setup
leantoken setup --claude --codex --yes
leantoken setup --all --yes
```

Supported clients are Claude Code, Cursor, OpenCode, Codex, Gemini CLI, and
Antigravity. Setup changes only each client's `leantoken` MCP entry—it does not
install skills, rules, or shell hooks. Remove the entries with the same client
selection flags:

```bash
leantoken remove --claude --codex --yes
leantoken --json remove --all --yes
```

Example host configuration:

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
