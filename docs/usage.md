# Usage and tool reference

LeanToken exposes the same retrieval services through its CLI and MCP server.
All paths are relative to the configured repository root, and all source
responses are bounded.

## Global options

```text
--root <PATH>      Repository root (default: current directory)
--database <PATH>  Override the per-repository SQLite cache path
--tokenizer <ENCODING>  Source and protocol accounting tokenizer
--json             Emit JSON from CLI commands
```

## CLI commands

```text
leantoken index [--rebuild]
leantoken status
leantoken files <tree|find|glob> [options]
leantoken search <query> [options]
leantoken outline <path>...
leantoken read <path> [--lines START:END] [--symbol NAME]
leantoken context --task <text> --budget <tokens>
leantoken mcp [--result-mode dual|text|structured]
```

Use `leantoken <command> --help` for the complete argument list.

## MCP server

`leantoken mcp` serves the five retrieval tools over stdio. It reconciles the
repository before serving, watches later filesystem changes, and reports
whether responses come from a current or reconciling index generation.

Logs go to stderr. Stdout is reserved for MCP protocol messages.

The default `dual` mode returns JSON as text and `structuredContent` for broad
host compatibility. `text` and `structured` remove that duplication, but use
them only after capturing the target host and confirming it consumes that
representation. The catalog publishes documented input schemas but omits
optional output schemas; repeating full response DTOs in every `tools/list`
result costs model context without changing tool behavior.

Prefer progressive retrieval:

```text
files -> outline/search -> exact read -> context only if scope remains uncertain
```

## `leantoken_files`

Discovers repository structure without returning source bodies.

Operations:

- `tree`: compact hierarchy with optional `path` and `depth` limits;
- `find`: fuzzy path and basename matching using `query`;
- `glob`: indexed path matching using `pattern`.

Common inputs are `max_results` and `cursor`. Output contains bounded
file/directory entries with language and size metadata when available.

## `leantoken_search`

Returns ranked source excerpts. Modes are `auto`, `text`, `regex`,
`identifier`, `symbol`, and `reference`.

Inputs include path filters, focus paths, result and token limits, context-line
count, case sensitivity, and a generation-bound cursor. Each hit includes its
path, one-based returned line range, excerpt, match kind, score reasons, and
content hash. Structural fields appear only when syntax supports them.

Lexical matches remain eligible when structural extraction is unavailable or
incomplete.

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
- `start_line` and `end_line` select a one-based range.
- `symbol` selects an indexed symbol range and cannot be combined with lines.
- `max_tokens` bounds returned source.
- `expected_hash` returns `not_modified` without source when it matches the
  hash from the same prior read range.

`content_hash` identifies the returned range. `indexed_hash` identifies the
whole indexed file. `index_stale` is true when the live file differs from the
indexed version (for example after an edit that has not been reindexed yet).
`meta.repository_generation` is the committed index generation used for path
and symbol lookup; `meta.freshness` is `reconciling` while an index operation
is active on this cache.

When the index has never completed a generation, tools other than status return
a retryable not-ready error rather than an empty success. After local edits,
prefer outline/search again once freshness is `current`, or trust `index_stale`
and re-read with `expected_hash` for unchanged ranges.

## `leantoken_context`

Turns a task into a ranked set of source evidence under `token_budget`.

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
terminating the server. Internal storage and I/O failures are logged without
including source bodies and are returned as generic MCP internal errors.

Default limits include:

- 2 MiB maximum indexed file size;
- 20 default and 100 maximum results per request;
- 80 lines or 32 KiB per search chunk;
- up to eight indexing workers;
- 64 KiB query input and 4 KiB path/pattern input.
