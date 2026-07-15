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
cargo build --release
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

The checked-in representative run found all seven labeled files while
returning 1,574 source tokens, compared with 28,870 tokens for full contents of
the labeled files. It covered only 3 of 18 labeled line anchors, so this is
evidence of a smaller source payload, not proof that retrieval quality is
solved. See the benchmark documentation for the corpus, methodology, and
limitations.

## License

MIT OR Apache-2.0
