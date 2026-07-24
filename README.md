<div align="center">

<h1>LeanToken</h1>

**Token-bounded repository context for coding agents**

Give agents the source they need without repeatedly sending whole files.

<img src="assets/leantoken-hero.png" alt="LeanToken distilling a large source repository into focused, token-bounded context" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![npm downloads](https://img.shields.io/npm/dm/leantoken?logo=npm&label=downloads)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[Install](#quick-start) · [Why LeanToken](#why-leantoken) · [Tools](#available-tools) · [CLI](#cli-usage) · [How it works](#how-it-works) · [Docs](#documentation)

</div>

---

## Quick start

Add LeanToken to Claude Code, Cursor, OpenCode, Codex, Gemini CLI, or
Antigravity:

```bash
npx leantoken setup
```

If `npx` reuses a stale project-local or ancestor install, run
`npx leantoken@latest setup` once to bootstrap the newest published release.

The setup wizard labels supported clients it detects, but leaves every client
unselected so you choose exactly which coding agents receive LeanToken. Before
writing anything, it shows the exact configuration paths and MCP launcher and
asks for confirmation. An npx-based setup pins the exact LeanToken version
that ran setup, so restarting a client cannot silently move to a newer release.

Global setup never stores the repository where setup happened. OpenCode gets a
workspace-relative working directory; other supported clients launch LeanToken
from the workspace cwd selected by the host. If a host instead starts it from
the home directory or a filesystem root, LeanToken refuses to index that broad
root by default.

Restart or reload the configured clients, then verify the complete MCP
handshake and first retrieval from a repository:

```bash
npx leantoken doctor
```

Start with a broad task such as: *Use LeanToken to map the relevant repository
context before editing.* The MCP initialization guidance routes the agent to
`leantoken_context` first and keeps native tools available for edits, builds,
and tests.

<table>
<tr>
<td width="33%" valign="top">
<strong>Local by default</strong><br><br>
Source is indexed on your machine in SQLite. LeanToken is a read-only discovery
and retrieval layer.
</td>
<td width="33%" valign="top">
<strong>Explicit token budgets</strong><br><br>
Every source response is bounded, so repository context does not quietly crowd
out the task.
</td>
<td width="33%" valign="top">
<strong>Built for agent workflows</strong><br><br>
Browse paths, search identifiers, inspect outlines, read exact ranges, and
inspect cumulative savings through six focused MCP tools.
</td>
</tr>
</table>

To skip the wizard, select clients explicitly or configure all supported
clients:

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

Use `--private-runtime` to copy the exact package-native executable into
LeanToken's versioned application-data directory and configure clients to
launch it directly, without persistent npm/Node wrappers. Preview its path and
digest with `--dry-run`.

Automation never treats detection as consent: `--yes` requires explicit client
flags, `--all`, or `--refresh` for entries already managed by LeanToken. Preview
the same resolved plan without changing files:

```bash
npx leantoken setup --codex --cursor --dry-run
```

Setup adds the `leantoken` MCP entry plus a small owned discovery skill in the
host-standard user skill directories. The skill advertises routing metadata;
it does not duplicate tool schemas, add rules, or install shell hooks. Remove
the owned integration with:

```bash
npx leantoken remove
```

Refresh only existing LeanToken MCP entries after explicitly choosing a new
version, or use an older version to roll back:

```bash
npx --yes leantoken@latest setup --refresh --yes
npx --yes leantoken@0.1.8 setup --refresh --yes
```

## Why LeanToken

Repository exploration often starts with broad searches, whole-file reads, and
the same source being loaded again after a handoff. LeanToken replaces that
loop with progressive disclosure:

| Typical repository exploration | With LeanToken |
| --- | --- |
| Scan broad directory listings | Browse a compact, ignore-aware tree |
| Read entire files to find structure | Request signatures, definitions, imports, and ranges |
| Repeat searches after handoffs | Suppress unchanged evidence with content hashes |
| Let source reads grow with file size | Apply an explicit token limit to every retrieval |
| Guess which files matter | Rank task-specific evidence when scope is uncertain |

The host agent still owns editing, commands, tests, conversation state, and
model orchestration. LeanToken handles repository discovery and bounded source
retrieval.

## Available tools

| Tool | Purpose |
| --- | --- |
| `leantoken_context` | Default first call for broad tasks; rank relevant evidence under a token budget. |
| `leantoken_search` | Prefer over grep/rg for ranked text, regex, identifier, symbol, or reference search. |
| `leantoken_files` | Prefer over find/ls/glob for compact, ignore-aware path discovery. |
| `leantoken_outline` | Inspect definitions, signatures, imports, and ranges without whole-file reads. |
| `leantoken_read` | Prefer over cat/head/sed for one exact symbol or inclusive line range. |
| `leantoken_savings` | Report cumulative repository-local estimated source-token savings. |

Every retrieval tool accepts `consistency: "working_tree"` when completed edits
must be reconciled before the query. The default, `"committed"`, returns the
latest completed index generation without waiting for filesystem changes.

The catalog stays intentionally small because every tool description and
schema also consumes model context.

## CLI usage

Run LeanToken directly through `npx`:

```bash
npx leantoken status
npx leantoken savings
npx leantoken doctor
npx leantoken --root /path/to/repo search handle_request
```

Or use a globally installed binary:

```bash
npm install --global leantoken@latest

leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

`npm install leantoken` installs the command in the current project's
`node_modules/.bin`; it does not add `leantoken` to the shell `PATH`. Invoke a
project-local install through `npx leantoken`, a package script, or
`./node_modules/.bin/leantoken`.

Run the MCP server manually over stdio:

```bash
leantoken --root /path/to/repo mcp
```

<details>
<summary><strong>Manual MCP client configuration</strong></summary>

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

</details>

## Installation options

The npm package includes native binaries for:

- macOS on ARM64 and x64
- glibc Linux on ARM64 and x64
- Windows on x64

Installation does not run lifecycle scripts or download an executable from a
postinstall hook. Other targets, including musl Linux, must build from source.
Install Rust 1.95 or later and a native C/C++ toolchain, then run:

```bash
cargo install --git https://github.com/morluto/leantoken
```

## Updating

MCP entries created through npx are pinned to the exact version that ran setup.
They change only when setup is run again for selected clients or with
`setup --refresh`. The configured launcher may contact npm to obtain that exact
package, but it never falls forward to `@latest`. Removing the npm cache while
offline can therefore make startup fail rather than execute an unapproved
version.

For a globally installed CLI or a CLI installed with Cargo:

```bash
leantoken upgrade --check
leantoken upgrade --yes
```

`update` is an alias for `upgrade`. For a project-local npm installation,
update the dependency with npm:

```bash
npm install leantoken@latest
```

## Cache management

LeanToken keeps one SQLite cache per canonical repository in the platform cache
directory. Inspect usage and preview an age- or size-based cleanup before
applying it:

```bash
leantoken cache list
leantoken cache prune --older-than 30 --dry-run
leantoken cache prune --max-total-bytes 1073741824 --yes
```

Active MCP leaders and followers hold a lifetime lease and are skipped. A
missing repository is retained unless it also meets another criterion or
`--remove-missing-roots` is passed explicitly, because removable and offline
volumes can be temporarily unavailable. Cache commands never inspect or delete
an explicit `--database` outside the managed cache directory.

## How it works

```text
repository
    │
    ▼
ignore-aware discovery ──► syntax extraction ──► SQLite + FTS5 index
                                                    │
                                                    ▼
agent request ──► ranked / exact retrieval ──► token-bounded evidence
```

LeanToken indexes source once, then serves compact paths, ranked matches,
structural outlines, exact source ranges, and task-specific context. Content
hashes reduce repeated evidence across turns and model handoffs.

The primary metric is useful repository evidence delivered per model token.

## Documentation

| Guide | Contents |
| --- | --- |
| [Usage and tool reference](docs/usage.md) | Commands, MCP tools, request options, and examples |
| [Architecture and reliability](docs/architecture.md) | Components, data flow, storage, and failure behavior |
| [Roadmap](docs/roadmap.md) | Current direction and planned work |
| [Development and testing](docs/development.md) | Local setup, validation, and release workflow |
| [Benchmark methodology](benchmarks/README.md) | Token-economy measurements and interpretation |
| [Measurement harnesses](docs/measurement.md) | Experiment, wire-cost, and profiling tools |

## License

Licensed under either of the following, at your option:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)
