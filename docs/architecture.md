# Architecture and reliability

LeanToken is a headless retrieval service. The CLI and MCP adapters call the
same typed application services and contain no indexing or ranking logic.

```text
repository files
      |
      v
ignore-aware discovery -> chunking -> tree-sitter extraction
      |                                  |
      +-----------> SQLite <-------------+
                 files + FTS5
                 symbols + imports
                        |
               retrieval services
                  /             \
                CLI             MCP
```

## Ownership boundaries

- Repository files are the source of truth.
- SQLite is the only derived-state store and can be deleted and rebuilt.
- The indexing layer owns discovery, text preparation, syntax extraction, and
  conservative import resolution.
- The storage layer owns migrations, transactions, generations, and FTS5.
- Retrieval services own validation, freshness checks, ranking inputs, token
  limits, and response models. The public `Services` type lives in
  `services.rs` (startup, indexing, status, snapshot consistency, meta).
  Retrieval entrypoints and their implementations live together under
  `services/`: `files`, `search`, `context`, and `read`, with shared request
  validation in `validation`.
- The MCP adapter owns SDK types, protocol error translation, cancellation, and
  stdio lifecycle. It omits optional output schemas from the catalog and offers
  explicit dual, text-only, and structured-only result modes. Dual remains the
  compatibility default.

LeanToken does not implement JSON-RPC framing or MCP dispatch. Those remain in
the official Rust MCP SDK.

## Storage

SQLite stores repository metadata, files, text chunks, definitions, syntactic
references, imports, reverse import candidates, and an ordinary relational path
projection. External-content FTS5 tables provide word and trigram indexes over
chunks.

LeanToken does not serialize a separate in-memory index snapshot. In this
document, a request snapshot means a SQLite read transaction pinned to one
committed generation. Persisted SQLite generations are disposable derived state
and are reconciled against repository files by the indexing leader.

The connection is configured with:

- WAL journal mode;
- foreign keys;
- a bounded busy timeout;
- bundled SQLite with an FTS5 trigram startup probe;
- transactional schema migrations;
- prepared-statement caching within each request session;
- file/range, reverse-import, and path lookup indexes added through versioned migrations so existing
  databases receive the same query plan as new databases.

Repository-aware service startup binds each database to its canonical
repository root. Default cache paths are already repository-specific; an
explicit database path claimed by a different root is rejected before either
repository can reconcile it. Different repositories therefore have independent
database, lock, watcher, worker, and failure domains. Multiple agents on one
repository intentionally share the same cache and committed generations.

One repository-scoped operation lock serializes reconciliation across processes.
File preparation happens outside SQLite, then an immediate write transaction
verifies that the generation and config used to build the plan are still
current. A stale plan is discarded and recomputed. Replacements, deletions, and
generation advancement then commit together.

Each multi-step retrieval (search, context, outline, files, read) opens one
checked-out read-only connection from an established, bounded `r2d2_sqlite`
pool and holds a DEFERRED transaction for the request
(`ReadSession`). Under WAL that pins a single committed snapshot for every
query in the assembly, so concurrent publishers cannot mix generations inside
one response. SQLite busy/locked errors while opening and pinning a snapshot
are retried a few times; generation zero returns a typed `IndexNotReady` error
instead of an empty success.

Structural search and context assembly pass bounded range/location sets through
SQLite JSON table-valued inputs. SQLite joins hydrate excerpts and enclosing
symbols in batches inside the same request snapshot; LeanToken keeps only the
domain-specific candidate fusion, overlap, and token-selection policy in Rust.

MCP retrieval inputs expose an explicit consistency boundary. `committed`, the
default, opens the latest completed snapshot immediately. `working_tree` first
runs a non-rebuild reconciliation under the repository-scoped operation lock,
then opens the resulting committed snapshot. This makes filesystem changes
completed before reconciliation visible without exposing a partially prepared
generation. Changes written concurrently may require a later request.

## Indexing and freshness

Discovery follows Git-compatible ignore rules, skips symlinks and oversized or
binary files, and normalizes indexed paths to forward slashes. Text files are
hashed, chunked on UTF-8 boundaries, and parsed in a bounded Rayon pool.

MCP starts the stdio protocol before opening SQLite or indexing. It answers the
mandatory initialize exchange first, then starts repository services after the
client's initialized notification. A generation-zero tool call returns a
successful structured `status: "retryable"` result rather than a tool error or
an empty retrieval result. An existing
complete generation remains queryable while its replacement is prepared.

Cache initialization, schema migration, and managed-cache corruption recovery
run under a separate repository-scoped initialization lock. SQLite busy and
locked results are retried with bounded backoff and caller-owned cancellation;
terminal startup failures move MCP tools to an unavailable state. The stdio
adapter supervises the indexing runtime for the lifetime of the connection, so
an unexpected runtime exit cannot leave tools permanently reporting startup.

MCP processes sharing one cache compete for a repository-scoped leadership
lock. The leader alone owns automatic indexing and one filesystem watcher;
followers normally read the same committed SQLite generations without scanning
or watching. An explicit `working_tree` retrieval may reconcile from any
process under the shared operation lock. Followers retry leadership
periodically, so an operating-system lock release after process exit provides
failover without a PID lease or stale-lock cleanup.

The leader registers its watcher before the initial reconciliation. Events that
arrive during discovery or parsing remain queued and are applied after the
commit, closing the startup event gap without an unconditional second full
walk. Later events are debounced and coalesced; ambiguous rename sequences,
backend rescan requests, and queue overflow request a full reconciliation.

For existing regular files, the watcher reconciles only the reported paths.
New paths, directory changes, symlinks, ignore-file changes, configuration
changes, and ambiguous deletions fall back to full discovery. Path-set
expansions query the indexed `import_candidates` reverse projection so only
importers whose bounded candidate paths gained or lost membership are reparsed.
New targets can therefore resolve previously unresolved edges without scanning
every stored import. Both the watcher path and full discovery
content-hash files before treating them as unchanged: matching size and mtime
alone never skips reindexing when the body changed (bind mounts, copy tools that
preserve mtime, some network filesystems). File replacement, deletion,
reverse-import invalidation, and generation advancement commit in one SQLite
transaction.

Indexing is serialized across processes, but queries continue against the last
committed WAL generation. The short-lived operation lock makes `reconciling`
visible to followers as well as the leader. Watcher and reconciliation tasks
receive caller-owned cancellation and are joined during shutdown.

Each `Services`/`Indexer` instance can own one Rayon worker pool sized from that
instance's `max_index_workers`. The pool is built lazily on the first non-empty
file preparation and reused afterward. Read-only followers therefore allocate
no indexing threads, while a process that becomes leader retains its configured
worker bound without rebuilding a pool on every reconciliation.

## Retrieval hot-path bounds

These limits cap context fan-out, regex work, and file-list memory. A request
returns `LimitExceeded` instead of silently returning incomplete regex results
when a scan boundary is reached. Tree pages use the indexed `path_entries`
projection and a path keyset cursor. Find and glob retain bounded page state but
still scan indexed files because their application matchers do not map to tree
ordering. The numbers are safety limits, not monorepo performance claims.

| Path | Bound |
| --- | --- |
| Context query terms | 12 (`MAX_CONTEXT_QUERIES`) |
| Context hits per term/source | 20 symbols/refs, 30 FTS |
| Regex match candidates | `min(max_results × 20, 2000)` |
| Regex files scanned | 10_000 |
| Regex chunks per file | 256 |
| File scan page size | 1_000 for regex/find/glob; tree queries `max_results + 1` projected paths |

Regex mode verifies patterns over snapshot file pages without materializing the
repository path list. Prefer symbol/identifier/text modes when a full-repo scan
is unnecessary. Compiled regex size and DFA cache are also limited so
pathological patterns fail closed.

Run the reproducible hot-path profile with, for example,
`cargo run --example hot_path_bounds --release -- --files 10000 --iterations 20`.
It reports warm p50/p95 wall time; run the command under `/usr/bin/time -v` when
process CPU and peak RSS are required. Results are host-local and should only be
compared on the same machine and release profile.

## Live read vs index

`leantoken_read` always reads the live filesystem for the returned body while
symbol resolution and path admission use the index. Responses include:

- `meta.repository_generation` — committed generation used for index lookups;
- `meta.freshness` — `current` or `reconciling` (local activity or the shared
  operation lock);
- `content_hash` — hash of the returned live range;
- `indexed_hash` — hash of the whole indexed file;
- `index_stale` — true when the live file body differs from the indexed file.

When `index_stale` is true, agents should re-outline or re-search with
`consistency=working_tree` if the next retrieval must include those edits. Pass
`expected_hash` on rereads to suppress unchanged ranges. Search and outline
never invent empty successful results at generation zero.

## Concurrency design constraints

- WAL permits concurrent readers but remains a single-writer database; it is
  not a work-deduplication mechanism.
- SQLite busy timeouts and retries are defensive handling, not index ownership.
- A process-local mutex cannot protect a cache shared by several MCP clients.
- Only the leader creates a watcher and index worker pool; one of each per MCP
  client would recreate the startup stampede outside SQLite.
- Lock files are stable cache artifacts and are never deleted on unlock. The
  open locked handle is the authority; PID files and heartbeat rows are not
  used as mutexes.
- Explicit and managed cache paths resolve through the deepest existing
  ancestor before missing descendants are appended. Database, WAL, SHM, and
  lock artifacts therefore share one identity even below symlink aliases and
  cannot enter repository discovery or watcher reconciliation.
- A repository root is persisted in cache metadata. Canonical aliases of the
  same root share it; a different root cannot reuse that database explicitly.
- Connection capacity remains per process/repository. The bounded established
  pool reuses read-only connections and prepared statements; it is not a global
  multi-repository coordination mechanism.
- Retrieval never exposes a partially built generation, and generation zero is
  never rendered as a successful empty repository.
- Automatic work does not delay the MCP initialize response, and startup does
  not invent unsolicited MCP progress tokens.

## Parsing

Tree-sitter extracts syntax facts for Rust, Python, JavaScript, TypeScript/TSX,
and Go. LeanToken stores flat definitions, syntactic references, signatures,
parents, and imports; syntax trees are discarded after indexing.

Syntax is not semantic resolution. A reference result means that a grammar
identified a reference-like occurrence. It does not prove the runtime target,
dynamic caller, type relationship, or safety of a refactor. Malformed files
remain text-searchable and are marked structurally incomplete.

## Retrieval and ranking

- Word FTS5 supplies identifier and term candidates.
- Trigram FTS5 narrows substring candidates.
- Rust `regex` verifies regex matches over indexed chunks.
- Symbol and syntactic-reference tables provide structural candidates.
- Conservative local-import edges can add a bounded number of neighboring
  files for orientation.

Ranking combines exactness, structural role, FTS relevance, path evidence,
fragment size, lexical frequency, optional focus, import proximity, change
generation, and a bounded working-tree signal. Qualified identifiers and
header-like terms are retained exactly, while part of the twelve-query budget
is reserved for high-value prose terms. Identifier expansions are added
round-robin so one long name cannot consume the budget. Reciprocal-rank fusion
applies only when a path matches multiple independent explicit terms; variants
of one identifier do not count as separate evidence. Signals change ordering;
absent structural evidence never removes a lexical match.

Symbol and lexical matches expand to the complete enclosing declaration when it
fits. Oversized declarations use a bounded window centered on the exact match,
so an arbitrary declaration prefix cannot hide the decisive line. Context
selection first covers independent task concepts, then prefers a second source
view on the selected definition path before filling by score. This keeps SQLite
chunking and candidate order from silently truncating known evidence.

Context selection hashes and deduplicates overlapping candidates, omits known
hashes, applies a relative confidence floor and per-file diversity cap, and
selects only complete fragments that fit the source-token budget. Fragment
hashes live once in an aligned receipt table rather than repeating beside every
fragment.

Each service instance owns its tokenizer configuration. Exact OpenAI BPE
encodings use `tiktoken-rs` singleton vocabularies; the explicit estimate mode
is marked inexact. Protocol-cost benchmarks serialize the actual tool catalog,
JSON-RPC requests and responses, result wrappers, and repeated-context handoff
instead of adding a guessed fixed overhead.

## Path and data safety

All repository-facing paths are relative. Absolute paths, parent traversal,
NUL bytes, and canonical paths outside the repository root are rejected.
Symlink escapes are rejected when live content is opened. `leantoken_read`
requires an indexed path, so ignore rules also govern which files can be read
through the tool.

LeanToken is read-only with respect to repository source. It does not execute
project commands or make network requests. Context ranking may invoke a bounded
`git status` process for an optional working-tree signal; timeout or failure
removes that signal. SQL values are parameterized. Logs contain paths, counts,
hashes, timings, and error summaries but not source bodies by default.

The index contains local source text in SQLite. Users should place an explicit
database path only where its filesystem permissions and retention policy are
appropriate for that repository.

## Failure behavior

- Request validation failures are typed and do not terminate MCP.
- Repeated generation changes are retryable repository conflicts, not invalid
  client parameters.
- Cancellation propagates from MCP request context into blocking retrieval
  loops and from MCP shutdown into initialization retries, lock waits,
  discovery, file preparation, result aggregation, and import resolution.
  Cancellation leaves the service usable for later calls.
- File replacement and multi-file reconciliation roll back on storage errors.
- Reconciliation publication rejects stale baseline generations before making
  mutations.
- Committed WAL state survives process failure. Confirmed corruption in a
  LeanToken-owned cache is deleted and rebuilt; an explicitly configured
  caller-owned database is preserved and the error is returned.
- EOF and orderly cancellation stop stdio service, watcher, and reconciliation
  tasks without detached worker threads. If the leader exits, a follower takes
  ownership and reconciles before resuming automatic watching.
