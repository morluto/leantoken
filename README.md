<div align="center">

# LeanToken

**Code intelligence for agents, with explicit source-token budgets.**

`mcp-name: io.github.morluto/leantoken`

<img src="assets/leantoken-hero-v3.jpg" alt="LeanToken narrowing a repository to the code an agent needs" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

</div>

LeanToken builds a local, immutable repository generation and serves bounded
search, outline, and read operations from that generation. The CLI and MCP
server share the same application services. Repository source stays local.

## Quick start

Configure a supported coding client:

```bash
npx leantoken setup
```

Restart the client, open a repository, and verify the connection:

```bash
npx leantoken doctor
```

For direct CLI use:

```bash
npx leantoken refresh
npx leantoken search RepositoryGeneration
npx leantoken outline src/storage/snapshot.rs
npx leantoken read src/storage/snapshot.rs --lines 1:120
```

LeanToken refuses filesystem roots, home directories, and parents of the home
directory unless `--allow-broad-root` is explicitly supplied. Setup previews
the files it will change and asks before writing; automation must select clients
explicitly.

## Retrieval model

`refresh` is the publication boundary:

```text
repository files -> bounded acquisition -> complete derived generation
                 -> atomic publish -> search / outline / read
```

A retrieval observes one committed generation. It never mixes files from two
publications. Watchers and compatibility reconciliation modes may request a
refresh, but a query does not become correct by racing the filesystem. Call
`refresh` when the working tree changes.

The public `read` operation returns indexed generation content. Library callers
that deliberately need dirty working-tree bytes use the separately named
`Services::read_worktree` operation and accept its weaker guarantees.

One MCP process is configured for one repository root. Start another process
for another repository; process boundaries provide honest resource and failure
isolation.

## Available tools

| Tool | Purpose |
| --- | --- |
| `leantoken.files` | Discover indexed paths without loading source. |
| `leantoken.search` | Search text, regex, identifiers, symbols, and references. |
| `leantoken.outline` | Inspect definitions, signatures, imports, and ranges. |
| `leantoken.read` | Read an exact indexed range, symbol, or heading. |
| `leantoken.history` | Inspect bounded Git history and diffs. |
| `leantoken.json` | Query bounded live JSON structures. |
| `leantoken.context` | Assemble ranked evidence for a task. |
| `leantoken.receipt_rebase` | Rebase immutable evidence onto a newer generation. |
| `leantoken.savings` | Report observed retrieval and token accounting. |

`files`, `search`, `outline`, and `read` are the retrieval kernel. `context`
orchestrates those primitives. Git, JSON, setup, updates, cache administration,
and offline analysis have distinct owners even when the executable projects
them through one CLI.

## CLI usage

See the [usage guide](docs/usage.md) for the current command surface and
`leantoken <command> --help` for exact options in the installed version.

## Token and result limits

Source budgets and serialized-response budgets are separate. A source budget
limits returned repository text; `--max-response-tokens` limits the complete
JSON response. Pagination cursors are bound to the repository, generation,
normalized request, and position, so a cursor cannot silently continue against
different content or parameters.

Search and context can omit content hashes already held by the caller.
Retrieval evidence and query proofs are immutable content-addressed artifacts,
not mutable conversational sessions.

## Setup and lifecycle

Select clients non-interactively with explicit flags:

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

Preview changes with `--dry-run`. An npx setup pins the exact version that ran
the command. To move managed entries to a chosen release:

```bash
npx --yes leantoken@latest setup --refresh --yes
```

Remove managed client entries with `npx leantoken remove`. Inspect managed
caches with `npx leantoken cache list` and private runtimes with
`npx leantoken runtime list`; pruning is explicit and supports `--dry-run`.

## Installation

The zero-install path is npm/npx. Native archives are published with GitHub
releases, and the Rust crate is published on crates.io. The npm package contains
prebuilt platform binaries and does not download executables in a postinstall
script.

## Documentation

| Document | Audience |
| --- | --- |
| [Usage](docs/usage.md) | Current CLI and MCP behavior |
| [Architecture](docs/architecture.md) | Generation, storage, and boundedness contracts |
| [Development](docs/development.md) | Test ownership and contribution workflow |
| [Benchmarking](benchmarks/README.md) | Opt-in evaluation tooling and evidence policy |
| [Releases](docs/releases.md) | Publication and recovery process |
| [Changelog](CHANGELOG.md) | Released changes |

Planning belongs in GitHub issues and pull requests. Historical designs and
experiment narratives remain available through Git history rather than being
maintained as current documentation.

## License

Licensed under either the MIT License or Apache License 2.0, at your option.
