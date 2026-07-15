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
  limits, and response models.
- The MCP adapter owns SDK types, protocol error translation, cancellation, and
  stdio lifecycle. It returns structured results while omitting optional output
  schemas from the catalog to avoid charging every client for duplicate DTOs.

LeanToken does not implement JSON-RPC framing or MCP dispatch. Those remain in
the official Rust MCP SDK.

## Storage

SQLite stores repository metadata, files, text chunks, definitions, syntactic
references, and imports. External-content FTS5 tables provide word and trigram
indexes over chunks.

The connection is configured with:

- WAL journal mode;
- foreign keys;
- a bounded busy timeout;
- bundled SQLite with an FTS5 trigram startup probe;
- transactional schema migrations;
- file/range lookup indexes added through a versioned migration so existing
  databases receive the same query plan as new databases.

One serialized writer applies a repository reconciliation. File preparation
happens outside the transaction, then replacements and deletions commit as one
generation. Readers open short read-only connections and retry a response when
the repository generation changes while it is being assembled. A returned
response therefore does not mix committed generations.

## Indexing and freshness

Discovery follows Git-compatible ignore rules, skips symlinks and oversized or
binary files, and normalizes indexed paths to forward slashes. Text files are
hashed, chunked on UTF-8 boundaries, and parsed in a bounded Rayon pool.

MCP startup performs an initial reconciliation, starts the watcher, and then
performs a catch-up reconciliation to close the startup event gap. Changes
observed during the catch-up pass are already queued by the watcher. Later
events are debounced and coalesced; ambiguous rename sequences, backend rescan
requests, and queue overflow request a full reconciliation.

For existing regular files, the watcher reconciles only the reported paths.
New paths, directory changes, symlinks, ignore-file changes, configuration
changes, and ambiguous deletions fall back to full discovery. Path-set
expansions also reparse unchanged importers so a newly added target can resolve
previously unresolved edges. Reported files are content-hashed even when size
and modification time are unchanged. File replacement, deletion,
reverse-import invalidation, and generation advancement commit in one SQLite
transaction.

Indexing is serialized, but queries continue against the last committed WAL
generation. Responses report `reconciling` while any reconciliation is active.
Watcher and reconciliation tasks are cancelled and joined during shutdown.

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

Symbol declaration excerpts may cross storage-chunk boundaries, but remain
capped by the request budget and range limit. This keeps SQLite chunking an
indexing detail rather than silently truncating a known syntactic range.

Context selection hashes and deduplicates overlapping candidates, omits known
hashes, applies a per-file diversity cap, and selects only complete fragments
that fit the source-token budget.

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
- Cancellation propagates from MCP request context into blocking retrieval
  loops and leaves the service usable for later calls.
- File replacement and multi-file reconciliation roll back on storage errors.
- Committed WAL state survives process failure; corrupt derived state can be
  removed and rebuilt without touching source.
- EOF and orderly cancellation stop stdio service, watcher, and reconciliation
  tasks without detached worker threads.
