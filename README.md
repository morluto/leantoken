# LeanToken — token-bounded repository context for coding agents

LeanToken gives coding agents the relevant parts of a repository without
repeatedly reading whole files. It runs locally as a read-only CLI and MCP
server, indexes source in SQLite, and keeps retrieval within explicit token
budgets.

## MCP setup — no global command installed

Set up LeanToken for your coding agents with one command:

```bash
npx leantoken setup
```

The npm package downloads the precompiled LeanToken binary for glibc-based
Linux (x64 or arm64), macOS (Intel or Apple Silicon), or Windows x64. The wizard
detects installed clients, lets you choose which ones to configure, and
registers LeanToken as a global MCP server. Each client launches it in the
active workspace.

This is a zero-install setup: it does not add a global `leantoken` command.
Run one-off CLI commands with `npx leantoken <command>`. The generated MCP entry
uses `leantoken` without a version pin rather than recording an ephemeral npm
cache path, so MCP clients follow current npm releases automatically.

Supported clients are Claude Code, Cursor, OpenCode, Codex, Gemini CLI, and
Antigravity. To skip the wizard, select clients explicitly or configure all of
them:

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

Setup installs only the `leantoken` MCP entry. It does not add skills, rules,
or shell hooks. Remove the generated entries later with:

```bash
npx leantoken remove
npx leantoken remove --all --yes
```

## Global CLI installation

Install LeanToken globally when you want to run `leantoken` without the `npx`
prefix:

```bash
npm install --global leantoken
leantoken --version
leantoken setup
```

The npm package downloads the native binary for the current platform. The MCP
configuration remains unpinned and updates independently from this persistent
CLI installation.

## Cargo installation

To build from source instead, install Rust 1.95 or later and a native C/C++
toolchain, then run `cargo install --git https://github.com/morluto/leantoken`.

## Updating

MCP entries created by `npx leantoken setup` use the unversioned npm package,
so they follow the npm `latest` release automatically. Older entries that
contain a version such as `leantoken@0.1.0` can be migrated once by rerunning
setup with the latest package:

```bash
npx leantoken@latest setup --codex --yes
```

For a persistent npm or Cargo installation, `update` and `upgrade` are
equivalent:

```bash
leantoken update
# or
leantoken upgrade
```

Check without installing, or skip the interactive confirmation:

```bash
leantoken upgrade --check
leantoken upgrade --yes
```

When run through npx, `upgrade` never installs LeanToken globally. It reports
whether a newer release exists and points to `npx leantoken@latest <command>`.

LeanToken detects whether the executable was installed through npm or Cargo
and delegates the update to that package manager. If the installation method
cannot be detected, run the matching command directly:

```bash
npm install --global leantoken@latest
cargo install --git https://github.com/morluto/leantoken --force
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
