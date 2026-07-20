# Usage and tool reference

LeanToken exposes the same retrieval services through its CLI and MCP server.
All paths are relative to the configured repository root, and all source
responses are bounded.

## Global options

```text
--root <PATH>      Repository root (default: current directory)
--allow-broad-root Allow a filesystem root, home directory, or parent of home
--include-generated Include known generated and package-cache directories
--max-walk-entries <COUNT>       Walker entries per discovery (default: 500000)
--max-files <COUNT>              Admitted source files (default: 150000)
--max-total-source-bytes <BYTES> Aggregate source bytes (default: 2147483648)
--max-depth <DEPTH>              Repository-relative depth (default: 64)
--max-file-bytes <BYTES>         Bytes admitted from one file (default: 2097152)
--max-prepare-batch-files <COUNT>  Files per preparation batch (default: 256)
--max-prepare-batch-bytes <BYTES>  Bytes per preparation batch (default: 67108864)
--database <PATH>  Override the per-repository SQLite cache path
--tokenizer <ENCODING>  Source and protocol accounting tokenizer
--json             Emit JSON from CLI commands
```

## CLI commands

```text
leantoken index [--rebuild]
leantoken status
leantoken doctor
leantoken files <tree|find|glob> [options]
leantoken search <query> [options]
leantoken outline <path>...
leantoken read <path> [--lines START:END] [--symbol NAME]
leantoken context --task <text> --budget <tokens>
leantoken mcp [--result-mode dual|text|structured]
```

Use `leantoken <command> --help` for the complete argument list.

## First-run doctor

`leantoken doctor` launches the current executable as a real MCP subprocess and
verifies its initialization identity and agent instructions, exact five-tool
catalog, and first `leantoken_context` retrieval. On a cold repository it
follows structured `retry_after_ms` guidance until the first index generation
is ready. Use `--json` for a machine-readable readiness report.

## MCP server

`leantoken mcp` starts the stdio protocol before opening the repository cache so
the initialize handshake is never blocked by indexing. After the client's
initialized notification, one process becomes indexing leader and followers
reuse its committed SQLite generations. Retrieval calls made before the first
generation commits return successful structured retry guidance with a reason
and `retry_after_ms`. Later calls report whether they use a current or
reconciling index generation.

LeanToken refuses to index a filesystem root, the current user's home directory,
or a parent of that home directory by default. This prevents an MCP host launched
from a broad working directory from recursively watching and indexing unrelated
projects and package caches. Select the workspace with `--root`; use
`--allow-broad-root` only for a deliberate broad index.

Repository discovery also fails closed when any configured walk-entry, file,
aggregate-byte, or depth limit is crossed. LeanToken returns a typed error and
keeps the previously committed generation intact; it never publishes a
truncated repository. Every numeric limit must be positive, and the preparation
batch byte limit must be at least the per-file byte limit. Limit failures stop
automatic MCP indexing until the process is restarted with a narrower root or
adjusted limits, preventing a fixed tree from being rescanned every 500 ms.

Discovery keeps useful hidden repository content, including `.github`,
`.devcontainer`, root dotfiles, and `.cargo/config.toml`. It skips known
generated and cache trees such as `node_modules`, `target`, `.venv`, `venv`,
`.tox`, `.cache`, package-manager caches, Python caches, `.gradle`, and
`.rustup`. Use `--include-generated` only when those trees are intentional
source inputs.

Place `.leantokenignore` files at the repository root or in nested directories
to add gitignore-style rules. They have higher precedence than `.gitignore` and
`.ignore`; negation rules can therefore restore paths hidden by those files.
Built-in generated-tree exclusions run before ignore matching, so restoring
those requires `--include-generated`. Changes to any ignore control file cause
one bounded visibility reconciliation.

Logs go to stderr. Stdout is reserved for MCP protocol messages. LeanToken
service errors exposed through MCP use fixed, allowlisted messages and a stable
`data.category` for client handling. Repository, database, and external
canonical paths, plus underlying I/O and SQLite details, remain in stderr
diagnostics rather than protocol responses.

The default `dual` mode returns JSON as text and `structuredContent` for broad
host compatibility. `text` and `structured` remove that duplication, but use
them only after capturing the target host and confirming it consumes that
representation. The catalog publishes documented input schemas but omits
optional output schemas; repeating full response DTOs in every `tools/list`
result costs model context without changing tool behavior.

Prefer LeanToken over shell discovery and whole-file reads. For a broad coding,
debugging, review, or architecture task, start with `leantoken_context`. Use the
narrow tools directly when the target is already known:

```text
broad task -> context
known identifier/text -> search -> read
known file, unknown range -> outline -> read
unknown path -> files
```

All five MCP retrieval tools accept an optional `consistency` input:

- `committed` (default) queries the latest completed index generation without
  waiting for filesystem changes;
- `working_tree` first reconciles the current working tree, then queries the
  resulting committed generation.

Use `working_tree` when edits, generated files, branch changes, or external
commits must be visible to the current call. Reconciliation uses the same
ignore rules and cross-process operation lock as automatic indexing, and the
request remains cancellable. Writes that begin concurrently with the call may
require another `working_tree` request. CLI users can run `leantoken index`
immediately before retrieval when they need to reconcile first.

## `leantoken_files`

Discovers repository structure without returning source bodies.

Operations:

- `{"kind":"tree","path":"src","depth":2}`: compact hierarchy;
- `{"kind":"find","query":"mcp"}`: fuzzy path and basename matching;
- `{"kind":"glob","pattern":"src/**/*.rs"}`: indexed path matching.

Pass one of those tagged objects as `operation`. Operation-specific fields
cannot be mixed. Common inputs are `max_results` (default 20, maximum 100) and
`cursor`. Output contains bounded file/directory entries with language and size
metadata when available.

## `leantoken_search`

Returns ranked source excerpts. Modes are `auto`, `text`, `regex`,
`identifier`, `symbol`, and `reference`.

Inputs include path filters, focus paths, result and token limits, context-line
count, case sensitivity, and a generation-bound cursor. Defaults are 20 results,
8,000 source tokens, and two context lines. Each hit includes its
path, one-based returned line range, excerpt, match kind, score reasons, and
content hash. Structural fields appear only when syntax supports them.

Lexical matches remain eligible when structural extraction is unavailable or
incomplete.

Regex search has explicit file, chunk, candidate, and compiled-program safety
limits. If a limit would make the answer incomplete, the tool returns
`LimitExceeded`; use text, identifier, symbol, or reference mode for exhaustive
indexed lookup on larger repositories.

## `leantoken_outline`

Returns definitions, imports, signatures, parent relationships, and one-based
line ranges for one or more files. Name and kind filters narrow the output.
Bodies are not returned by default.

Supported languages report whether parsing was structurally complete.
Unsupported text files remain searchable and are marked incomplete rather than
being presented as precise.

## `leantoken_read`

Reads an exact source range.

- `path` is required.
- `target: {"kind":"lines","start":40,"end":90}` selects an inclusive
  one-based range.
- `target: {"kind":"symbol","name":"LeanTokenMcp"}` selects one indexed
  symbol definition.
- `max_tokens` defaults to 8,000 and is capped at 32,000.
- `expected_hash` returns `not_modified` without source when it matches the
  hash from the same prior target.

`content_hash` identifies the returned range. `indexed_hash` identifies the
whole indexed file. `index_stale` is true when the live file differs from the
indexed version (for example after an edit that has not been reindexed yet).
`meta.repository_generation` is the committed index generation used for path
and symbol lookup; `meta.freshness` is `reconciling` while an index operation
is active on this cache.

When the index has never completed a generation, retrieval tools return a
successful retry result such as `{"status":"retryable","reason":"index_building",
"retry_after_ms":500}`. Retry the same call after that delay. After local edits,
set `consistency` to `working_tree` on the next MCP retrieval. A committed read
may still use `index_stale` and `expected_hash` to detect or suppress live ranges.

## `leantoken_context`

Turns a task into a ranked set of source evidence. `task` is the only required
input; `token_budget` defaults to 3,000 and is capped at 32,000.

Optional inputs focus or exclude paths and symbols, provide hashes already held
by the caller, and identify a prior repository generation. The selector merges
overlapping candidates, suppresses duplicate or known content, preserves file
diversity, and returns short reasons for each chosen fragment.

The evidence receipt contains a task fingerprint and a compact hash list aligned
by index with the returned fragments. Repository generation appears once in
response metadata. The receipt is returned but not persisted. Passing its
`fragment_hashes` as `known_hashes` prevents those exact fragments from being
resent; other relevant evidence may still be returned.

For a frontier-to-executor handoff, transfer the grounded fragments, receipt,
repository generation, current todo list, and first validated edit. This is a
compact trajectory manifest, not a LeanToken session. The executor can pass the
receipt hashes back without rereading the same evidence.

CLI equivalents make the reuse contract explicit:

```bash
leantoken --json read src/lib.rs --lines 40:90 --expected-hash HASH
leantoken --json context --task "finish the validated fix" --budget 1200 \
  --known-hash HASH_FROM_RECEIPT --prior-generation 7
```

## Token accounting

`search`, `outline`, `read`, and `context` bound returned source text. The
default read limit is 8,000 tokens and the hard source-output ceiling is 32,000
tokens.

`emitted_tokens` counts source text with the configured tokenizer. The default
is `cl100k_base`. Exact built-in modes are `cl100k_base`, `o200k_base`,
`o200k_harmony`, `p50k_base`, `p50k_edit`, `r50k_base`, and `gpt2`.

`estimate` is an inexact heuristic for providers whose tokenizer is not
available locally. It does not guarantee that a provider will accept a payload
at the reported budget; responses mark this with `token_count_exact: false`.

Source limits do not include JSON keys, paths, scores, hashes, receipts, tool
schemas, or JSON-RPC envelopes. The benchmark utilities report those costs
separately rather than presenting source-token counts as complete MCP cost.

Every source range has a 128-bit BLAKE3 fingerprint for local identity and
duplicate suppression. Direct search/read responses carry it with the range;
context places hashes once in the aligned receipt table. Receipts transfer
grounded context without creating a LeanToken session, transcript, or model
state.

## Errors and limits

Oversized inputs, invalid regular expressions or globs, stale cursors,
unsupported structured reads, and unsafe paths return request errors without
terminating the server. Their MCP `data.category` values are stable enough for
client branching, while messages never echo caller-supplied or resolved paths.
Internal repository configuration, storage, and I/O failures are logged without
including source bodies and are returned as generic MCP internal errors.

Default limits include:

- 2 MiB maximum indexed file size;
- 20 default and 100 maximum results per request;
- 80 lines or 32 KiB per search chunk;
- up to eight indexing workers;
- 64 KiB query input and 4 KiB path/pattern input.
