# Usage

LeanToken can run as a CLI or an MCP stdio server. Both adapters call the same
typed services and use the same repository-generation database.

## Global options

- `--root <PATH>` selects the repository. The default is the current directory.
- `--database <PATH>` overrides the repository-scoped managed database.
- `--json` emits compact JSON.
- `--tokenizer <ENCODING>` selects exact or estimated token accounting.
- `--allow-broad-root` permits a filesystem root, home directory, or parent of
  home that LeanToken otherwise rejects.
- `--include-generated` includes known generated and package-cache paths.
- `--index-include <PATTERN>` and `--index-exclude <PATTERN>` define immutable
  index scope.
- `--max-walk-entries <COUNT>` (default: 500000)
- `--max-files <COUNT>` (default: 150000)
- `--max-total-source-bytes <BYTES>` (default: 2147483648)
- `--max-depth <DEPTH>` (default: 64)
- `--max-file-bytes <BYTES>` (default: 2097152)
- `--max-prepare-batch-files <COUNT>` (default: 256)
- `--max-prepare-batch-bytes <BYTES>` (default: 67108864)
- `--max-index-workers <COUNT>` bounds source-preparation workers.

Repository configuration lives in `.leantoken.toml`. Context excludes these
generated evidence paths by default:

```text
artifacts/runtime_reports/**
artifacts/viability_audit/**
artifacts/replay_reports/**
notes/runs/**
node_modules/**
```

## CLI commands

leantoken refresh [--rebuild]
leantoken status
leantoken coverage
leantoken savings
leantoken files <tree|find|glob> [OPTIONS]
leantoken search <QUERY> [OPTIONS]
leantoken outline [PATHS] [OPTIONS]
leantoken read <PATH> [OPTIONS]
leantoken history <read|diff|trace> [OPTIONS]
leantoken json <query|summary|compare> [OPTIONS]
leantoken context --task <TEXT> [OPTIONS]
leantoken doctor
leantoken mcp [--result-mode <MODE>]
leantoken setup [CLIENTS] [OPTIONS]
leantoken remove [CLIENTS] [OPTIONS]
leantoken cache <list|prune> [OPTIONS]
leantoken runtime <list|prune> [OPTIONS]
leantoken episode [OPTIONS]
leantoken update [OPTIONS]
leantoken upgrade [OPTIONS]

Use `leantoken <command> --help` for the complete, version-matched argument
reference. The rest of this guide explains contracts that are easy to miss in
generated help.

## Refresh and freshness

Run `leantoken refresh` after edits, branch changes, generated output, or other
filesystem changes that retrieval must observe. `--rebuild` discards all
derived projections and constructs the generation from scratch.

Every index-backed retrieval pins one completed generation. Watcher delivery is
an optimization that can request a refresh; it is not the correctness boundary.
The legacy `reconcile_working_tree` consistency value means
refresh-before-query. Prefer an explicit refresh followed by
`indexed_generation` retrieval in new integrations.

## Core CLI examples

```bash
leantoken files tree --path src --depth 2
leantoken files find --query storage
leantoken search RepositoryGeneration --mode symbol
leantoken search 'TODO|FIXME' --mode regex --all-occurrences
leantoken outline src/storage/snapshot.rs --max-tokens 800
leantoken read src/storage/snapshot.rs --symbol RepositoryGeneration
leantoken read README.md --heading "Retrieval model"
leantoken context --task "find generation publication and its tests" --budget 3000
```

`read` returns generation-backed source. It accepts a line range, exact symbol,
or document heading and supports bounded continuation. Dirty source is not
silently substituted. The live `Services::read_worktree` operation is currently
a Rust API, not an MCP tool or canonical CLI read mode.

`history` reads bounded immutable Git objects. `json` deliberately reads live
JSON and reports that boundary; neither pretends to be part of an indexed
generation.

## MCP server

Run `leantoken mcp` from the repository root. One server process serves one
repository. The host should start another process for another root.

The default result mode is `structured`. `dual` repeats JSON in text and
structured content for hosts that require both; `text` is a compatibility
fallback. MCP transport and lifecycle use RMCP 3.1.2. LeanToken adds bounded
stdio admission and product-specific error translation around the SDK.

The catalog contains these nine tools.

## `leantoken.files`

Discover indexed paths with a tagged `tree`, `find`, or `glob` operation. It
returns paths and metadata, not source. Use `projection: "paths"` for the
smallest shape.

## `leantoken.search`

Search with `auto`, `text`, `regex`, `identifier`, `symbol`, or `reference`
semantics. Compact projections return source-free coordinates. Exhaustive text
or regex occurrence mode fails closed if the configured scan boundary cannot
prove completeness.

When a request consults structural references, `reference_capability` states
that extraction is partial: zero structural hits are not proof that no callers
or occurrences exist. Use lexical `identifier` or `text` search where a
language, construct, or incomplete parse falls outside the structural adapter.

## `leantoken.outline`

Return definitions, signatures, imports, parse coverage, and source ranges for
known files. Use returned symbols or ranges with `read`.

## `leantoken.read`

Read an exact range, symbol, or heading from the pinned generation. Supply
`expected_hash` to receive `not_modified` without source when the caller already
holds the same content. Continuation cursors are generation- and request-bound.

## `leantoken.history`

Read, diff, or trace bounded symbols and paths across Git revisions. Results
come from Git objects, not the mutable index.

## `leantoken.json`

Query, summarize, or compare bounded JSON structures. This tool reads the named
live file and reports parsing and pagination limits explicitly.

## `leantoken.context`

Assemble ranked task evidence under a source-token budget. `plan_only` returns
candidate metadata without source. Focus and must-include constraints fail
loudly when the bounded candidate set cannot satisfy them. Prefer caller-held
`known_hashes` for repeated work.

The compatibility consistency values are `indexed_generation` and
`reconcile_working_tree`; new hosts should call refresh explicitly. Default
context exclusions are listed under [Global options](#global-options).

## `leantoken.receipt_rebase`

Carry exact path, coordinate, and hash evidence from one immutable generation
artifact into another. Non-matching evidence is classified rather than guessed.
This is explicit orchestration, not mutable retrieval-session state.

## `leantoken.savings`

Return best-effort aggregate source and response accounting. Counters are
instrumentation, not correctness state or a billing ledger.

## Cursors, hashes, and budgets

A cursor commits to its format version, repository, generation or content,
normalized request, and typed position. Changing any of those inputs invalidates
the cursor.

`max_tokens` bounds returned source. `max_response_tokens` bounds the complete
serialized response. If metadata alone cannot fit, LeanToken returns a typed
minimum instead of silently dropping required fields. Known hashes and
`expected_hash` suppress repeated source without changing query semantics.

## Setup, cache, and updates

Interactive setup detects supported clients, shows exact configuration changes,
and asks before writing. Non-interactive setup requires explicit client flags,
`--all`, or `--refresh`. Use `--dry-run` to inspect the plan. Managed entries
pin the chosen version; they do not advance automatically.

Cache and runtime pruning are explicit administrative operations. Preview them
with `--dry-run`; ordinary retrieval never evicts a cache or runtime. `update`
and `upgrade` are aliases for selecting a newer release, after which managed
client entries must be refreshed explicitly.

## Errors and limits

Invalid requests, not-ready indexes, stale generations, cursor mismatches,
resource limits, response-budget minima, cancellation, and unavailable
instrumentation are distinct typed outcomes. MCP semantic failures are tool
errors; transport failures remain protocol errors. Host paths and infrastructure
details stay in local diagnostics rather than model-visible error messages.
