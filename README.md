<div align="center">

<h1>LeanToken</h1>

**Make every AI coding token go further**

Local-first code intelligence for coding agents. Search code, inspect structure,
read exact ranges, and explore Git history through a CLI and MCP server.

<img src="assets/leantoken-hero-v3.jpg" alt="LeanToken narrowing a large codebase to the files and code an AI agent needs" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![npm downloads](https://img.shields.io/npm/dm/leantoken?logo=npm&label=downloads)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[Install](#quick-start) · [Why LeanToken](#why-leantoken) · [Tools](#available-tools) · [CLI](#cli-usage) · [How it works](#how-it-works) · [Docs](#documentation)

</div>

---

> **Measured token savings:** In a controlled 60-run study, LeanToken used
> 20.1% fewer model input tokens than the agent's built-in tools with limited
> repository exploration, and 37.6% fewer than those tools with broad
> exploration. See exactly
> how it was measured in the [measurement methodology](docs/measurement.md).

## Quick start

Add LeanToken to Claude Code, Cursor, OpenCode, Codex, Gemini CLI, or
Antigravity:

```bash
npx leantoken setup
```

<details>
<summary><strong>Setup behavior and safety</strong></summary>

Current releases stop setup before writing when `npx` resolves a stale
project-local or ancestor install, and point to
`npx leantoken@latest setup`. Older releases that predate this check can be
bootstrapped directly with that versioned command.

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

</details>

Restart or reload the configured clients, then verify the connection and first
retrieval from a repository:

```bash
npx leantoken doctor
```

Try a broad task such as: *Find the code related to request cancellation before
editing.* LeanToken helps the agent start with `leantoken.context`, while its
normal tools remain available for edits, builds, and tests.

Check how many tokens LeanToken has saved:

```bash
npx leantoken savings
```

<table>
<tr>
<td width="33%" valign="top">
<strong>Local by default</strong><br><br>
Source is indexed on your machine in a local database. LeanToken is a read-only
discovery and retrieval layer.
</td>
<td width="33%" valign="top">
<strong>Explicit token budgets</strong><br><br>
Every response has an explicit token limit, so large files cannot take over the
request.
</td>
<td width="33%" valign="top">
<strong>Built for agent workflows</strong><br><br>
Find files, search code, inspect structure, read exact ranges, trace history,
query JSON, and track token usage through focused tools.
</td>
</tr>
</table>

<details>
<summary><strong>Advanced setup and version management</strong></summary>

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
npx --yes leantoken@0.1.8 setup --refresh --yes --allow-outdated
```

</details>

## Why LeanToken

Most agents start by searching widely and reading whole files. LeanToken narrows
that work in stages:

| Typical repository exploration | With LeanToken |
| --- | --- |
| Scan broad directory listings | Find relevant paths in a compact tree |
| Read whole files to find structure | See definitions and imports without loading the entire file |
| Send the same code again after each turn | Avoid repeating unchanged evidence |
| Let large files fill the request | Keep returned source within an exact source-token budget and report response overhead separately |
| Guess which files matter | Rank likely relevant code for the task |

Your coding agent still handles editing, commands, tests, and conversation.
LeanToken finds and returns the code those tasks need.

LeanToken does not create one giant prompt file. It answers focused searches and
reads as the agent needs them.

### Example

For a task like *fix request cancellation during shutdown*, an illustrative
bounded result might look like this:

```text
Budget: 1,200 source tokens

Selected evidence:
  src/services/executor.rs        lines 137-147, 251-259
  src/services/reconciliation.rs lines 148-175, 257-272
```

The agent receives these ranges instead of both full files. If the budget is too
small, the response also says what was left out. Paths, scores, receipts, JSON,
and MCP transport wrappers are not part of this source-token budget; see
[token accounting](docs/usage.md#token-accounting) for the measured boundaries.

## Available tools

| Tool | Purpose |
| --- | --- |
| `leantoken.context` | Default first call for broad tasks; preview or materialize ranked evidence under a token budget. |
| `leantoken.search` | Prefer over grep/rg for ranked text, regex, identifier, symbol, or reference search. |
| `leantoken.files` | Prefer over find/ls/glob for compact, ignore-aware path discovery. |
| `leantoken.outline` | Inspect definitions, signatures, imports, and ranges without whole-file reads. |
| `leantoken.read` | Prefer over cat/head/sed for one exact symbol or inclusive line range. |
| `leantoken.history` | Read, diff, or trace one parsed symbol across immutable Git revisions. |
| `leantoken.json` | Query, summarize, or compare bounded live JSON with paged keys and typed diagnostics. |
| `leantoken.savings` | Report source compression and cumulative full-response token accounting. |

<details>
<summary><strong>Advanced retrieval controls</strong></summary>

Every index-backed retrieval tool accepts `consistency: "reconcile_working_tree"` when
completed edits must be reconciled before the query. The default,
`"indexed_generation"`, returns the latest completed index generation without
scanning or waiting for filesystem changes; it is not a Git revision boundary.
`leantoken.history` reads immutable Git objects and `leantoken.json` reads exact
live files, so neither accepts an index consistency mode. To constrain context
to immutable history, pass `BASE..HEAD` as `leantoken.context.base_revision`
with `strict_changed_paths: true`.

For an uncertain broad task, set `plan_only: true` to receive bounded ranked
candidate metadata without source fragments or receipt mutation. Confirm the
paths and coverage, then repeat the same request with `plan_only: false` to
materialize the selected source. Context omission diagnostics are compact by
default; set `verbose_diagnostics: true` only when full path, file-type, reason,
score-band, focus, and changed-path facets are needed.

</details>

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

MCP entries created through `npx` stay pinned to the exact LeanToken version
that configured them. Update existing client integrations explicitly:

```bash
npx --yes leantoken@latest setup --refresh --yes
```

For a globally installed CLI or a CLI installed with Cargo:

```bash
leantoken upgrade --check
leantoken upgrade --yes
```

`update` is an alias for `upgrade`. For a project-local npm installation:

```bash
npm install leantoken@latest
```

Pinned MCP entries never silently move to `@latest`. If the exact package is not
available locally or online, startup fails rather than selecting another version.
Updating the CLI does not change existing MCP entries. See the
[usage guide](docs/usage.md) for rollbacks, cache management, and version details.

## Cache management

Inspect local repository caches or preview cleanup before applying it:

```bash
leantoken cache list
leantoken cache list --summary
leantoken cache prune --older-than 30 --dry-run
leantoken cache prune --max-total-bytes 1073741824 --yes
```

See the [usage guide](docs/usage.md) for cache states, pagination, and cleanup
safety rules.

## How it works

```text
repository
    │
    ▼
file discovery ──► code structure extraction ──► local search index
                                                    │
                                                    ▼
agent request ──► ranked / exact retrieval ──► focused code within a token budget
```

LeanToken indexes source once, then serves compact paths, ranked matches,
structural outlines, exact source ranges, and task-specific context. It avoids
resending unchanged evidence across turns.

LeanToken's goal is to return the code an agent needs with fewer input tokens.

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
