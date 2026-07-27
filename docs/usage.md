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
--max-index-workers <COUNT>        Parallel file-preparation workers
--database <PATH>  Override the per-repository SQLite cache path
--tokenizer <ENCODING>  Source and protocol accounting tokenizer
--json             Emit JSON from CLI commands
```

## CLI commands

```text
leantoken index [--rebuild]
leantoken status
leantoken savings
leantoken doctor
leantoken files <tree|find|glob> [options] [--consistency <mode>]
leantoken search <query> [options] [--consistency <mode>]
leantoken outline <path>... [--consistency <mode>]
leantoken read <path> [--lines START:END] [--symbol NAME] [--consistency <mode>]
leantoken history <operation> ... [options]
leantoken json <path> [options]
leantoken context --task <text> --budget <tokens> [--consistency <mode>]
leantoken update [--check] [--yes]
leantoken upgrade [--check] [--yes]
leantoken mcp [--result-mode dual|text|structured]
leantoken setup [CLIENT...] [--all] [--refresh] [--yes] [--dry-run] [--allow-outdated]
leantoken remove [CLIENT...] [--all] [--yes] [--dry-run]
leantoken cache list [--summary] [--state STATE] [--repository-root PATH]
                     [--limit COUNT] [--cursor CURSOR]
leantoken cache prune [--older-than DAYS] [--max-total-bytes BYTES]
                      [--remove-missing-roots] [--dry-run] [--yes]
```

Use `leantoken <command> --help` for the complete argument list.

`leantoken status` reports readiness separately from reconciliation activity.
`index_state` is `uninitialized` until the first generation commits and `ready`
afterward. `freshness` is `current` while idle and `reconciling` while an index
operation is active, so a cold idle repository reports
`uninitialized`/`current`. Status is deliberately read-only and reports
`working_tree_checked: false`; `current` describes reconciliation activity, not
a filesystem scan. Before the first generation, direct CLI retrieval exits with
guidance to run `leantoken index`; use `leantoken doctor` to verify the complete
MCP startup and first-retrieval flow. Status also reports SQLite main/WAL/SHM
bytes, indexed source bytes, their amplification ratio, and current process RSS
when the platform exposes it. RSS is per process, not a claim about all clients
sharing the repository cache. `index_content_version` identifies the managed
cache compatibility lane used by the current binary; different values use
separate managed cache paths.

After the first generation, the one-shot `files`, `search`, `outline`, `read`,
and `context` commands default to `--consistency reconcile_working_tree`. Each
command completes a non-rebuild reconciliation before opening one committed
snapshot, so edits completed before the command are visible atomically. Use
`--consistency indexed_generation` when a lower-latency query of the latest
completed snapshot is intentional. Changes written concurrently may require a
later request.

`leantoken savings` reports persistent repository-local source compression and
full-response accounting. The backward-compatible source-only fields cover
successful `search`, `outline`, `read`, and materialized `context` responses.
Search, outline, and context compare emitted source with whole-file reads of
the unique represented files. Read compares the emitted range with the
requested live range before truncation or suppression.

The nested `response_accounting` object additionally covers `files`,
`context_plan`, `json`, and `history`. It separates source,
path/metadata, protocol, and total compact-response tokens. JSON uses the
complete input file or files as its represented-source baseline. Operations
without a defensible source baseline still contribute their full response cost,
so their signed `estimated_net_tokens_saved` value is negative rather than
silently disappearing. Counts are stored separately per configured tokenizer.

The default terminal view presents an aligned summary and per-operation table,
using color when stdout is a terminal. `NO_COLOR` or `CLICOLOR=0` disables
color, while `CLICOLOR_FORCE=1` enables it for compatible redirected output.
Pass `--json` for the stable compact JSON representation used by scripts.

Full-response counts include the compact structured response but not tool
discovery, JSON-RPC transport envelopes, provider billing/cache behavior,
pre-response failures, native-tool costs, or task/evidence success. Successful
retries are counted as separate requests but are not grouped into tasks.
Accounting is best effort: a busy repository writer skips telemetry rather
than delaying or failing retrieval. Whole-file baselines are also unavailable
when the selected tokenizer does not match the indexed tokenizer until
reconciliation completes. Source-only counters from older caches remain
visible, but cannot be reconstructed as historical full-response costs and are
therefore excluded from `response_accounting`. This is not an audit ledger.

Current index responses retain the aggregate `files_skipped` count and explain
it with the bounded `skip_reasons` object: `binary`,
`oversized_during_read`, and `failed`. These counts cover files admitted for
preparation and always sum to `files_skipped`. Legacy serialized responses can
omit the object because their breakdown is unknown. No per-file skip list is
returned; bounded failure warnings may still identify files that could not be
read. `files_seen` counts admitted files plus deletions directly observed from
requested targeted paths. Paths merely omitted by full or visibility discovery
because they are absent or excluded are not part of `files_seen`,
`files_skipped`, or the reason counts. An already-indexed omitted path can still
increment `files_removed` when its stale entry is deleted.

## MCP setup and version lifecycle

Setup writes only the `leantoken` entry in each selected global client config.
It also manages a concise `leantoken` discovery skill in
`~/.agents/skills/leantoken/SKILL.md` and
`~/.claude/skills/leantoken/SKILL.md`. Hosts preload only its name and routing
description, then load the instructions on selection; the eight MCP schemas
remain deferred. Repeated setup updates only marker-owned copies, removal
preserves an unowned file at either path, and partial client removal retains the
skill while another LeanToken registration remains. JSON setup reports the
exact `cl100k_base` size of one discovery skill as telemetry; it is not a
pass/fail cap on the routing guidance.
When setup runs through npx, the stored command pins
`leantoken@<exact current version>` and retains `--yes` so background MCP
startup cannot block on an install prompt. The launcher may contact npm to
resolve or download that exact package, but it cannot switch to a newer version
between restarts.

To avoid retaining npm and Node wrapper processes for every MCP session, select
the private native runtime explicitly:

```bash
npx --yes leantoken@0.1.10 setup --codex --private-runtime --dry-run
npx --yes leantoken@0.1.10 setup --codex --private-runtime --yes
```

Dry-run reports the exact versioned application-data path and BLAKE3 digest.
Setup copies the native executable already verified by the running package,
activates it with an atomic no-clobber rename, then updates all selected client
registrations as one rollback-capable transaction. A process lock serializes
setup, and a durable journal restores pre-transaction contents on the next
setup invocation after an interruption; recovery refuses to overwrite a file
changed independently after the interruption. The registered command launches
that native executable directly. Removal deletes registrations but retains
versioned runtimes for explicit rollback; it never selects `latest`.

Choose upgrades and rollbacks explicitly by running the desired version, then
refresh only entries that already exist:

```bash
npx --yes leantoken@latest setup --refresh --yes
npx --yes leantoken@0.1.8 setup --refresh --yes
```

`setup --refresh --dry-run` audits the same plan without writing. Refresh does
not infer consent from installed clients and does not create new entries. If an
exact package is neither cached nor reachable while offline, startup fails; it
does not fall forward to `@latest`.

Global setup does not bind the repository where setup was run. OpenCode's
entry uses workspace-relative `cwd: "."`. Claude Code, Cursor, Codex, Gemini
CLI, and Antigravity use the working directory their host assigns to the MCP
process, which must be the active workspace. Broad home and filesystem roots
still fail closed before cache creation or indexing. `--root` remains available
for deliberate manual or project-scoped configurations.

## Managed cache lifecycle

`cache list` inspects every recognized per-repository, per-index-content-version
cache in the platform `ProjectDirs` cache directory and reports exact aggregate
counts and bytes. Per-cache entries are returned in stable identifier order,
20 at a time by default and at most 100 at a time. Pass `--cursor` with the same
filters to continue. `--summary` omits entries, repeatable `--state` values are
OR filters, and `--repository-root` matches one exact recorded root. Entry pages
include compatibility version, recorded root, schema, last access, direct
SQLite/sidecar bytes, metadata state, and active lease status. Legacy
repository-only cache identities remain visible and prunable. Listing does not
open repository services and therefore works from any directory. JSON output
contains Unix timestamps and returned/matched/total counts for automation.

The versioned identity applies to automatically managed caches. An explicit
`--database` path remains unchanged and must not be shared concurrently by
incompatible index-content versions.

`cache prune` requires at least one explicit selection policy:

- `--older-than DAYS` selects caches whose last repository bind is at least that
  old;
- `--max-total-bytes BYTES` selects least-recently-used caches until the managed
  total reaches the requested bound;
- `--remove-missing-roots` explicitly selects a cache when its recorded root is
  currently absent.

Use `--dry-run` to inspect every keep/delete/skip decision. Actual deletion
requires `--yes`. Missing roots are not an implicit deletion criterion because
offline mounts and removable volumes can return later. Corrupt, incomplete, and
older-schema caches remain listable and can be selected by age or size. A cache
with a newer schema, mismatched root identity, or unexpected directory content
is always skipped.

Every `Services` instance holds a shared lease from before SQLite initialization
until its final clone drops. Prune must acquire the exclusive form and therefore
skips active MCP leaders, followers, and CLI services. It deletes the database,
WAL, SHM, journal, and coordination sidecars but retains the zero-byte lease
identity so a returning repository cannot race a new process through a replaced
lock file. Explicit `--database` files outside the managed directory are never
enumerated. Stop older LeanToken versions that predate cache leases before
pruning during a mixed-version rollout.

## First-run doctor

`leantoken doctor` launches the current executable as a real MCP subprocess and
verifies its initialization identity and agent instructions, exact eight-tool
catalog, and first `leantoken.context` retrieval. On a cold repository it
allows the first retrieval's bounded internal wait, then follows structured
`retry_after_ms` guidance if the index needs longer. Use `--json` for a
machine-readable readiness report, including the executable's
`index_content_version`. This doctor launches the current executable; it does
not claim to identify other running binaries that share an explicit database.
Failures use the `doctor_failure` category and identify the `launch`,
`handshake`, `catalog`, or `first_retrieval` stage so repair tooling does not
need to parse prose.

## MCP server

`leantoken mcp` starts the stdio protocol before opening the repository cache so
the initialize handshake is never blocked by indexing. After the client's
initialized notification, one process becomes indexing leader and followers
reuse its committed SQLite generations. A retrieval call made while the first
generation is being built waits internally for up to 30 seconds so a short cold
index does not require another model turn. If no generation commits within that
bound, the call returns successful structured retry guidance with a reason and
`retry_after_ms`. Later calls report whether they use a current or reconciling
index generation.

One MCP server accepts at most 16 active tool calls. Its cloned request
handlers share that bound, while a separately started MCP process—including an
agent attached to another workspace—has independent capacity. Initialization
and `tools/list` remain available when tool calls are saturated. All tools,
including `savings`, use admission; a request for the wrong repository is
rejected before it consumes a slot.

Admitted retrieval work uses a separate Services executor with eight running
blocking closures and room for eight queued operations. A seventeenth active
operation returns retryable reason `retrieval_capacity_exhausted` immediately.
Queued work that cannot start within 500 ms returns
`retrieval_queue_timeout`. Cancelling queued work prevents its closure from
starting. Cancelling or aborting a caller whose closure is already running does
not make that capacity available early; it returns only when the closure exits.
These are per-server safety bounds, not a shared machine-wide quota.

Each retrieval snapshot holds one pooled SQLite connection and a DEFERRED WAL
read transaction. It does not copy the database or leave a snapshot file.
Snapshot memory is bounded by the eight running readers; up to eight additional
admitted requests may retain their bounded inputs while queued.
Long-running readers can temporarily delay WAL page reuse, so concurrency
changes should be evaluated with WAL and RSS measurements rather than by
raising the connection count alone.

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

The indexing leader registers its watcher before the initial scan so changes
during startup are not lost. Watcher queues and retained path state are bounded;
bursts collapse to one pending reconciliation. Automatic reconciliation waits
for a quiet period, and repeated full rescans or transient failures use capped
backoff. Terminal root, discovery-limit, configuration, and cache-binding
errors stop the indexing runtime and require a corrected configuration or
restart.

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

Search, outline, read, and context responses return an opaque
`meta.receipt_id`. Within one live MCP or programmatic service session, pass
that ID to a later retrieval to suppress exact content and overlapping source
ranges already returned. Near-duplicate evidence remains visible and increments
`meta.receipt_near_duplicates`. Receipts are bounded, server-managed, and tied
to one repository generation; unknown, evicted, and stale receipts fail
explicitly. Context `fragment_hashes` and `known_hashes` remain available for
stateless compatibility.

Prefer LeanToken over shell discovery and whole-file reads. For a broad coding,
debugging, review, or architecture task, start with `leantoken.context`. Use the
narrow tools directly when the target is already known:

```text
broad task -> context
known identifier/text -> search -> read
known file, unknown range -> outline -> read
unknown path -> files
```

All five MCP retrieval tools accept an optional `consistency` input:

- `indexed_generation` (default) queries the latest completed index generation
  without scanning or waiting for filesystem changes. It does not mean Git
  HEAD and may include files indexed from an earlier working-tree state;
- `reconcile_working_tree` first reconciles the current working tree, then
  queries the resulting completed generation.

Use `reconcile_working_tree` when edits, generated files, branch changes, or
external commits must be visible to the current call. Reconciliation uses the
same ignore rules and cross-process operation lock as automatic indexing, and
the request remains cancellable. Concurrent requests on one server share a
reconciliation wave when no scan has started yet. Requests arriving after a
scan starts wait for the next wave so they cannot inherit an older freshness
boundary. Cancelling one caller does not cancel shared work already running.
If that work fails, every coalesced caller receives the shared typed cause;
LeanToken does not automatically rerun the same failed scan for each caller.
Writes that begin concurrently with the call may require another
`reconcile_working_tree` request. CLI users can run `leantoken index`
immediately before retrieval when they need to reconcile first.

Numeric retrieval limits are inclusive and validated uniformly by the CLI,
MCP, and direct service APIs. `max_results` must be in `1..=100`;
`max_tokens` and `token_budget` must be in `1..=32,000`; `context_lines` may be
zero and must not exceed 20. Omitted optional values use their documented
defaults. Values outside these ranges are rejected rather than silently
clamped. Disallowed zero values are invalid input; values above a maximum
produce an MCP error with the public field name, requested value, and active
maximum.

LeanToken's stdio transport admits at most 16 decoded tool calls into the MCP
SDK at once and holds that capacity through response delivery; each server also
admits at most 16 active tool handlers. Excess calls fail fast with
`status: "retryable"` rather than creating unbounded SDK tasks. Initialization
and `tools/list` remain available while tool capacity is saturated. These
bounds are process-local: another MCP process serving another workspace has
independent capacity.

## `leantoken.savings`

Returns cumulative repository-local token accounting with no input fields.
Existing top-level fields retain the source-only estimate and its four
operation rows. `response_accounting` reports successful response counts,
comparable baseline counts, source/path-metadata/protocol/total response tokens,
signed net tokens saved, receipt-suppression counts, and fixed rows for all
eight retrieval operations.

The report is a represented-source comparison over successful recorded
responses. It does not observe failed calls, retry chains, whether returned
evidence was used, superseded calls, provider framing, or task success. Treat
the signed net value as local response accounting, not as a correctness-adjusted
claim about an agent's full workflow.

This is a read-only observation: calling `leantoken.savings` does not update
the tracker. Ask the host agent how many tokens LeanToken saved or request
LeanToken usage statistics to route directly to this tool.

## `leantoken.files`

Discovers repository structure without returning source bodies.

Operations:

- `{"kind":"tree","path":"src","depth":2}`: compact hierarchy;
- `{"kind":"find","query":"mcp"}`: fuzzy path and basename matching;
- `{"kind":"glob","pattern":"src/**/*.rs"}`: indexed path matching.

Pass one of those tagged objects as `operation`. Operation-specific fields
cannot be mixed. Common inputs are `max_results` (default 20, maximum 100) and
`cursor`. Output contains bounded file/directory entries with language and size
metadata when available.

## `leantoken.search`

Returns ranked source excerpts. Modes are `auto`, `text`, `regex`,
`identifier`, `symbol`, and `reference`.

Inputs include path filters, focus paths, result and token limits, context-line
count, case sensitivity, and a generation-bound cursor. Defaults are 20 results,
8,000 source tokens, and two context lines. Each hit includes its
path, one-based returned line range, excerpt, primary `match_kind`, all merged
`match_kinds`, score reasons, content hash, raw score, and a `normalized_score`
from 0 to 1 relative to the strongest candidate in the query. Structural fields
appear only when syntax supports them.

`auto` and `identifier` searches merge lexical and structural hits that resolve
to the same indexed definition coordinates. Set `prefer_structural=true` to
retain the structural definition excerpt as the primary hit when channels are
merged; merged channel and score-reason diagnostics are preserved either way.
The response `coverage` reports `total`, current-page `returned`, and
`truncated` counts separately for definitions, references, and text/regex
matches. One merged hit can represent more than one channel.

Set `all_occurrences` in `text` or `regex` mode to return one hit for every
non-overlapping match, including repeated matches in one indexed chunk or line.
Those hits include exact line and UTF-8 byte coordinates. The response reports
`occurrences_returned` for the current page and an exact `occurrences_total`
across the filtered index. Exhaustive pagination applies `max_results` and
`max_tokens` without changing the total; follow `next_cursor` until absent.

Each page examines at most `max_results` ranked candidates. `max_tokens` may
filter some or all of those candidates, so a page can contain fewer hits or be
empty while still returning `next_cursor`. Follow the cursor to examine later
candidates. When `next_cursor` is absent, every candidate was examined; increase
`max_tokens` and restart the search if omitted excerpts must become eligible.

Lexical matches remain eligible when structural extraction is unavailable or
incomplete.

Repository-wide lexical scans have explicit file, per-file chunk, occurrence,
and compiled-program safety limits. Exhaustive text and regex modes remove the
candidate-chunk cap, but retain the other limits. If a limit would make the
answer incomplete, the tool returns `LimitExceeded` instead of reporting a
partial exhaustive result.

## `leantoken.outline`

Returns definitions, imports, signatures, parent relationships, and one-based
line ranges for one or more files. Name and kind filters narrow the output.
Bodies are not returned by default.

`parse_complete` reports whether every requested file was parsed completely;
each file reports the same state independently. `structurally_complete` remains
as a compatibility alias on each file. Parse completeness does not imply result
completeness.

`result_complete` is true only when the response contains every filtered symbol
and import. Exact `total_symbols`, `returned_symbols`, `total_imports`,
`returned_imports`, and `symbol_counts_by_kind` make coverage auditable.
`truncated_by_max_results` provides `meta.next_cursor` for another page, while
`truncated_by_max_tokens` means the query must be repeated with a larger token
budget to recover omitted entries. Outline cursors are bound to the repository
generation, normalized path order, and symbol filters.

Supported languages report whether parsing was structurally complete.
JavaScript, TypeScript, and TSX outlines include top-level `const`, `let`, and
`var` bindings, exported data bindings, class fields, and object/array default
exports. Function-local variables remain lexical search evidence rather than
outline symbols.
C# outlines include namespace and type declarations plus methods, local
functions, constructors, properties, fields, events, enum members, indexers,
and operators. `using` directives are imports, method-like ranges include
complete bodies, and type and call references report their enclosing member.
CSS outlines include complete selector rules, custom properties, media/supports/
container conditions, and keyframes. Selector atoms are available to reference
search. HTML outlines include sectioning elements, IDs, forms and controls,
dialogs, buttons and links, `data-*` actions, hash anchors, and script/style
resources. HTML resource paths are also reported as imports.
Markdown outlines include ATX and Setext headings as `markdown_heading`
symbols. Their ranges cover the complete section through the line before the
next heading of equal or higher level, parent fields preserve the heading tree,
and headings inside fenced code blocks are excluded.
Unsupported text files remain searchable and are marked incomplete rather than
being presented as precise.

## `leantoken.read`

Reads an exact source range.

- `path` is required.
- `target: {"kind":"lines","start":40,"end":90}` selects an inclusive
  one-based range.
- `target: {"kind":"symbol","name":"LeanTokenMcp"}` selects one indexed
  symbol definition.
- `target: {"kind":"heading","name":"Installation"}` selects one indexed
  Markdown section. The exact outline signature form, such as
  `"name":"## Installation"`, is also accepted. Add `"occurrence":2` to
  select the second duplicate heading; occurrences are one-based and follow
  source order.
- `target: {"kind":"continuation","cursor":"..."}` continues a truncated
  response. The cursor preserves byte-exact progress even when the preceding
  page ended in the middle of a line.
- `max_tokens` defaults to 8,000 and accepts values through 32,000.
- `expected_hash` returns `not_modified` without source when it matches the
  hash from the same prior target.
- `delta: true` records a complete, non-truncated target as a bounded future
  base. On a changed follow-up, pass the prior `content_hash` as
  `expected_hash` and keep `delta: true`. The response uses `status: "delta"`
  and the `delta` field only when the complete unified diff costs fewer source
  tokens than full current content.

`content_hash` identifies the returned range. `indexed_hash` identifies the
whole indexed file. `index_stale` is true when the live file differs from the
indexed version (for example after an edit that has not been reindexed yet).
`target_start_line` and `target_end_line` describe the complete resolved target;
`returned_start_line` and `returned_end_line` describe the current page.
`status: "truncated"`, `truncated: true`, `next_start_line`, and
`continuation_cursor` fail loudly whenever source remains. Continuation cursors
are bound to the repository generation, path, and live full-file hash, so a
stale cursor cannot combine pages from different file versions.
`delta_receipt` reports the stable target key, base and head hashes and
generations, full and delta token counts, avoided tokens, and any explicit
fallback reason. Missing bases, changed target coordinates, truncated or
oversized content, and uneconomic diffs return full content. Delta state is
in-memory and repository-local, expires after 30 minutes, and is bounded to
128 entries, 512 KiB per entry, and 8 MiB of retained content. It never applies
to ranked context fragments or continuation cursors.

`status: "not_modified"` means `expected_hash` matched. The distinct
`status: "receipt_suppressed"` means a server-managed evidence receipt already
contained the exact current content. Changed content in an overlapping range
is returned and added to the receipt rather than being hidden as unchanged.
`meta.repository_generation` is the committed index generation used for path
and symbol lookup; `meta.freshness` is `reconciling` while an index operation
is active on this cache.

When the index has never completed a generation, retrieval tools wait for up to
30 seconds before returning a successful retry result such as
`{"status":"retryable","reason":"index_building","retry_after_ms":500}`. Retry
the same call after that delay. Caller cancellation interrupts the internal
wait. After local edits, set `consistency` to `reconcile_working_tree` on the
next MCP retrieval. An `indexed_generation` read may still use `index_stale`
and `expected_hash` to detect or suppress live ranges.

## `leantoken.json`

Reads exact repository-relative JSON files without requiring them to be indexed,
including ignored artifact paths. Operations are:

- `query`: select the root, an RFC 6901 JSON Pointer, or a standard JMESPath
  expression, then return `value`, `collapsed`, `keys`, or `schema`.
- `numeric_summary`: collect numeric leaves below the selection and return exact
  count, min, median, nearest-rank p95, max, and ignored non-numeric count.
- `diff_fields`: evaluate up to 100 selectors against two files and report
  presence, projected before/after values, and whether each field changed.

`collapsed` replaces arrays with their total count and a bounded sample.
`max_items` defaults to 1,000 (maximum 10,000), `array_sample_size` defaults to
3 (maximum 20), and `max_tokens` defaults to 8,000. Exact source hashes bind
responses to the complete live files. Raw values that exceed a cap fail loud.

`keys` is a deterministic flat projection and supports pagination under both
item and token limits. Every keys response reports exact `total_items`,
`returned_items`, and `remaining_items`. An incomplete page identifies
`max_items` or `max_tokens` in `incomplete_reason` and returns
`meta.next_cursor`; repeat the identical path, selector, and projection with
`cursor` to continue. The cursor is bound to those query inputs and the live
source hash, so a changed file or selector fails with `stale_cursor` instead of
mixing pages from different results.

Incomplete `schema`, `collapsed`, and projected diff results report the same
exact counts and `incomplete_reason`. Their nested shapes are not split into
ambiguous pages, so they do not return a cursor; increase `max_items` or use a
narrower selector. Strict JSON parse failures include the syntax category,
one-based line and column, and zero-based byte offset. JMESPath compile and
runtime failures include their stage, typed reason, expression offset, line,
and column.

## `leantoken.history`

Reads symbol-aware evidence from immutable Git revisions without changing the
working tree or index:

- `operation: {"kind":"read_symbol","path":"src/lib.rs","symbol":"Services",
  "revision":"main~1"}` parses the historical blob and returns that symbol.
- `operation: {"kind":"diff_symbol",...}` parses the symbol independently at
  `base_revision` and `head_revision`, then returns a bounded unified diff.
- `operation: {"kind":"symbol_log",...}` starts at `revision` (default `HEAD`)
  and uses Git line-history traversal for the resolved symbol range.

Historical paths are repository-relative, revisions are resolved before object
lookup, and blobs remain subject to the configured per-file byte limit.
Nested symbols use the same `parent.name` qualification accepted by exact live
reads. `diff_symbol` permits the file or symbol to be absent at one endpoint:
`before` or `after` is omitted, the unified diff contains the complete bounded
addition or deletion, and `semantic_change.kind` is `added` or `removed`. A
symbol absent at both endpoints remains a typed `symbol_not_found` error.
`max_tokens` defaults to 8,000 and applies to historical source or unified diff;
truncation is explicit through `result_complete`, `HistoricalSymbol.truncated`,
or `diff_truncated`. `max_results` defaults to 20 and is capped at 100 for
`symbol_log`. Symbol metadata includes the resolved 12-character revision,
complete line range, kind, parent, and full-content hash. This tool deliberately
has no index consistency mode because Git objects are immutable.

`diff_symbol` also returns `semantic_change` when the matched symbol content
differs. The receipt classifies the change as `modified`, distinguishes
`signature_changed` from `body_only`, and marks `public_contract_changed` only
when an explicitly `pub`, `public`, or `export` signature changes. Added and
removed exact symbols use the same public-contract rule. The unified diff
remains the source of truth. Renames are classified by immutable review context,
where both changed paths and symbol names can vary.

For context restricted to immutable history, pass `BASE..HEAD` as
`leantoken.context.base_revision` with `strict_changed_paths: true`. A single
commit uses `COMMIT^..COMMIT`; the resolved diff scope and coverage receipt make
the hard boundary explicit.

## `leantoken.context`

Turns a task into a ranked set of source evidence. `task` is the only required
input; `token_budget` defaults to 3,000 and accepts values through 32,000.

Optional inputs focus or exclude paths and symbols, provide hashes already held
by the caller, and identify a prior repository generation. `include_paths` is a
hard boundary: every returned source fragment must match at least one supplied
pattern, while `focus_paths` remains a ranking boost unless
`strict_focus_paths=true`. `minimum_fragments_per_focus_path` reserves the
requested number of fragments for every focus pattern before ordinary ranking.
`strict_changed_paths=true` restricts fragments to the resolved explicit paths,
an immutable `BASE..HEAD` range, a base-revision-to-working-tree diff, or current
Git working-tree changes when neither diff input is supplied. Include, strict
focus, strict changed, and exclude constraints are intersected; no constraint
silently broadens another. `must_include_paths` and
`must_include_symbols` generate and select required indexed evidence before
focus minimums and ordinary ranking. `max_fragments` defaults to 8 and accepts
values through 100.

Required symbols share the request token budget. A definition that fits its
share is returned completely. When it does not fit, the fragment and plan
candidate report `truncated: true` together with the complete
`target_start_line` and `target_end_line`; coverage reports the name under
`partial_must_include_symbols` instead of claiming complete coverage.

Set `plan_only=true` to run the same hard scopes, ranking, must-cover selection,
token budget, and fragment limit without returning source. The response has an
empty `fragments` array and no server-managed receipt mutation; `plan` contains
bounded paths and ranges, final scores and reasons, exact source-token
estimates, focus coverage, completeness, and a generated-artifact warning.
`receipt_id` is rejected in plan mode; use `known_hashes` for stateless
suppression that must apply to both preview and materialization.
After confirming a broad plan, repeat the same request with `plan_only=false`
to materialize those candidates against the selected index consistency
boundary.

Context ranking excludes known generated report trees by default:
`artifacts/runtime_reports/**`, `artifacts/viability_audit/**`,
`artifacts/replay_reports/**`, `notes/runs/**`, and `node_modules/**`. Exact
`files`, `search`, and `read` operations are unaffected. A matching
`include_paths` pattern explicitly admits an indexed artifact to context; an
explicit `exclude_paths` or strict scope still wins.

Repositories can append context-only exclusions in `.leantoken.toml`:

```toml
[context]
exclude_paths = ["generated/**", "reports/audit/**"]
```

These patterns do not remove files from the index. Known cache trees that are
not indexed by default, including `node_modules`, still require the global
`--include-generated` indexing override before exact lookup or explicit context
inclusion can find them. Repository configuration is resolved when LeanToken
opens the repository.

The `coverage` receipt distinguishes unmatched focus/include constraints,
covered requirements, indexed requirements blocked by path or budget limits,
and requirements absent from the index. Every focus path returns indexed and
selected fragment counts with an implicit minimum of one; strict or explicit
minimum requests additionally contribute to `strict_scope_satisfied`. Strict
changed-path requests return resolved and selected changed-path counts. An empty
strict scope therefore returns an explicit coverage failure rather than
unrelated evidence. Already-held matching hashes satisfy a must-cover
requirement without resending source.

`omission_summary` distinguishes path filtering, known hashes, and budget or
result limits with compact aggregate counts by default. Coverage, routing,
truncation warnings, and the bounded individual omission detail remain present,
so compact diagnostics do not weaken hard-scope or must-cover reporting. Set
`verbose_diagnostics=true` (`--verbose-diagnostics` in the CLI) to additionally
group omitted candidates by path, language or file type, reason, score band,
focus membership, and changed-path membership. Verbose facet lists are
deterministic and bounded to 12 values; longer path or file-type tails are
combined into `[other]`. Candidates rejected before scoring use the `not scored`
band. The selector merges overlapping candidates, suppresses duplicate or known
content, preserves file diversity, and returns short reasons for each chosen
fragment.

`workflow` accepts `auto`, `implementation`, `contribution`, `review`, or
`investigation`. Contribution and review modes add bounded repository guidance,
issue/PR templates, validation configuration, and tests whose names match
changed or focused source paths. `auto` selects a specialized mode only from
high-confidence task language; ordinary tasks retain implementation ranking.
The resolved mode is returned as `workflow`. Specialized responses include a
`workflow_receipt` with candidate counts and explicit missing evidence families;
missing guidance or owner tests is not represented as proof that none exists.

Materialized `review` requests over an immutable `BASE..HEAD` range also place
a `semantic_change` receipt under `diff_scope.evidence`. It deterministically
classifies parsed definitions as `added`, `removed`, `renamed`, or `modified`;
splits matched modifications into signature and body-only changes; identifies
explicit public-contract changes; reports recognized JSON configuration changes
as RFC 6901 key paths without values; and labels likely owner tests as `found`,
`missing`, or `unknown`. Rename requires one unique normalized body fingerprint
on each side. Ambiguity, incomplete parsing, byte or result limits, unsupported
Git entries, and truncated test scans are emitted as gaps rather than guessed.

Semantic classification shares the diff-evidence cap of 64 changed paths and
separately caps symbol and configuration changes at 64 each, with an 8 MiB
aggregate historical-content limit. It is absent from plan-only and non-review
responses. Review requests
against the working tree or a single base revision retain owner-test coverage
but report `semantic_change_requires_immutable_range`; no risk score is
produced.

Diff-scoped requests spanning at least 32 changed paths across three or more
deterministic path groups also return a bounded `routing` receipt. It reports
candidate, changed, selected, and group counts and suggests up to three narrower
`include_paths` scopes. The receipt records the originating consistency
boundary, base revision, and held fragment hashes once; callers overlay a
suggested scope while reusing the original diff inputs. It is decomposition
guidance, not a completeness claim.

The context evidence receipt retains a compact hash list aligned by index with
the returned fragments. For normal same-session reuse, pass
`meta.receipt_id` instead of copying those hashes. The server then suppresses
exact duplicates and overlapping ranges across context, search, outline, and
read. `fragment_hashes` plus `known_hashes` remains the stateless fallback for
clients that cannot retain a server receipt.

Set the optional `handoff` object on a materialized request when a host is about
to compact a broad context or transfer work to another executor. The response
then includes `handoff_manifest`: a source-free task summary, repository and
generation identity, Git commit and working-tree state, diff identities,
selected path/line/hash coordinates captured before receipt suppression, held
hashes, focus inputs, changed/related/test paths, and caller-supplied
validations, assumptions, questions, negative evidence, and avoid rules.
Validations are transported as caller reports; LeanToken does not execute them.
`plan_only` and `handoff` are rejected together because a plan has not
materialized grounded evidence.

The manifest is a host-triggered transfer artifact, not persistent memory.
`receipt_id` is useful only in the same server process; coordinates and hashes
remain the persistent verification boundary. If Git identity or working-tree
state cannot be established, the corresponding field is absent or `unknown`
and `gaps` explains the missing provenance. Receipt suppression can leave
`fragments` empty while the manifest still records the selected pre-suppression
coordinates.

Host state is capped at 16 validations and 16 entries in each note list.
Summaries and notes accept 512 UTF-8 bytes per item; validation commands accept
1,024. Output retains at most 100 evidence coordinates, 64 held hashes, 32
focus paths, 32 focus symbols, 64 changed paths, 64 related paths, 32 test
paths, and 64 explicit gaps. Deterministic truncation adds a gap rather than
claiming completeness. Because the manifest itself has protocol cost, request
it for genuine multi-fragment handoffs rather than routine small context calls;
the repository benchmark reports the zero-, one-, and all-reread crossover.

Every retrieval response also includes an opaque `meta.repository_id`. MCP
callers can pass it back as `expected_repository_id`; a server bound to another
repository or linked worktree rejects the call with
`repository_identity_mismatch` instead of returning misleading empty evidence.

CLI equivalents make the reuse contract explicit:

```bash
leantoken --json read src/lib.rs --lines 40:90 --expected-hash HASH
leantoken --json context --task "finish the validated fix" --budget 1200 \
  --known-hash HASH_FROM_RECEIPT --prior-generation 7
leantoken --json context --task "transfer the grounded implementation state" \
  --handoff --handoff-summary "Continue the validated parser fix"
```

## Token accounting

`search`, `outline`, `read`, and `context` bound returned source text. The
default read limit is 8,000 tokens and the hard source-output ceiling is 32,000
tokens. Assembled context has a separate 3,000-token default. Programmatic
configurations may lower these defaults and ceilings; omitted MCP fields use
the active service defaults rather than the static tool-schema examples.

Every retrieval response separates budgeted evidence from model-facing response
overhead:

- `source_tokens` counts the selected evidence text. Path-only `files`
  responses therefore report zero source tokens.
- `protocol_tokens` counts the compact JSON response envelope with scalar
  values neutralized and result arrays emptied.
- `path_and_metadata_tokens` counts the remaining non-source response cost,
  including paths, metadata values, and repeated result structure.
- `total_response_tokens` counts the final compact JSON service response,
  including the nonzero accounting fields themselves.
- `payload_tokens` is a compatibility alias for `total_response_tokens`.
- `tokenizer` identifies the tokenizer used for every count.

Accounting is filled repeatedly to a deterministic fixed point. Therefore
`source_tokens + protocol_tokens + path_and_metadata_tokens` equals
`total_response_tokens`, and tokenizing the final compact DTO produces that
exact total. Source limits remain independent and do not by themselves impose a
hard ceiling on the final serialization. Context callers can opt into that
second boundary with `ServiceCallOptions`, MCP `max_response_tokens`, or CLI
`--max-response-tokens`; a mandatory correctness skeleton that cannot fit
returns a typed limit error. These counts and limits describe the service
response DTO, not MCP
text/structured-content duplication, tool schemas, provider framing, or JSON-RPC
envelopes. In the default `dual` mode, MCP serializes the structured payload in
both text and `structuredContent`; use the wire-cost harness when that boundary
matters.
- `emitted_tokens` remains a compatibility alias for `source_tokens`.

The default tokenizer is `cl100k_base`. Exact built-in modes are `cl100k_base`,
`o200k_base`, `o200k_harmony`, `p50k_base`, `p50k_edit`, `r50k_base`, and
`gpt2`.

`estimate` is an inexact heuristic for providers whose tokenizer is not
available locally. It does not guarantee that a provider will accept a payload
at the reported budget; responses mark this with `token_count_exact: false`.

`savings` uses the same tokenizer and marks whether its local counts are exact.
The backward-compatible `estimated_source_tokens_saved` total still sums a
saturating per-request source-only difference. The signed
`response_accounting.estimated_net_tokens_saved` instead subtracts every
recorded complete response from the represented-source baseline, so metadata,
protocol, plan-only, discovery, and history costs can reduce the net result.
Only successful recorded responses contribute; failures, retries, evidence use,
and task outcomes are outside this report.

Source limits do not include JSON keys, paths, scores, hashes, receipts, tool
schemas, or JSON-RPC envelopes. `payload_tokens` captures the compact response
DTO costs; benchmark utilities continue to measure complete transport costs
when they include schemas, result-mode duplication, and JSON-RPC envelopes.

Every source range has a 128-bit BLAKE3 fingerprint for local identity and
duplicate suppression. Direct search/read responses carry it with the range;
context places hashes once in the aligned receipt table. Receipts transfer
grounded context without creating a LeanToken session, transcript, or model
state.

## Errors and limits

Failed CLI commands emit a human-readable `Error: ...` line by default. With
`--json`, they emit one compact JSON object on stderr and retain the existing
top-level `error` string for backward compatibility. The additive `category`
field is the stable machine-readable discriminator. Request errors may also
include the public `field`, `requested`, and active `limit`; clients should
branch on these fields instead of parsing `error` text.

Argument parsing failures use `invalid_input` and retain clap's exit status 2.
Help and version output are not failures: they remain on stdout with status 0,
even when `--json` is present. Errors after successful parsing retain status 1.
JSON mode suppresses tracing on stderr so each failure remains one complete JSON
document.

Current category values are:

| Category | Condition |
| --- | --- |
| `invalid_input` | Invalid values, missing arguments, or typed input validation |
| `input_too_long` | A public input crossed its byte limit |
| `invalid_request` | An audited caller-usage conflict |
| `request_limit_exceeded` | A result, token, context, or collection limit |
| `not_indexed` | A requested path or symbol is absent from the index |
| `index_not_ready` | No repository generation has committed yet |
| `stale_cursor` | A cursor is malformed or belongs to another request/generation |
| `request_cancelled` | Cooperative cancellation stopped the request |
| `path_outside_root` | A path escaped the repository boundary |
| `unsupported_language` | Structured parsing is unavailable for the language |
| `invalid_regex`, `invalid_glob` | Invalid pattern syntax |
| `repository_configuration` | Invalid root, cache binding, or configuration |
| `repository_index_limit` | Repository discovery crossed a hard bound |
| `runtime_unavailable` | A required runtime capability is unavailable |
| `retryable_conflict` | Concurrent repository state requires a retry |
| `internal_error` | An implementation, storage, I/O, or other unexpected failure |

The structured fields are an allowlist. I/O, SQLite, serialization, and other
unexpected failures use `category: "internal_error"` and expose no additional
machine-readable details. Future releases may add categories or optional
fields, so consumers should ignore keys and category values they do not know.

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
- up to four indexing workers by default; override with `--max-index-workers`;
- 64 KiB query input and 4 KiB path/pattern input.
