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
  compatibility default. Protocol errors cross an explicit allowlist: clients
  receive fixed safe messages and stable category data, while path-bearing and
  infrastructure details remain in stderr diagnostics.

LeanToken does not implement JSON-RPC framing or MCP dispatch. Those remain in
the official Rust MCP SDK.

## Storage

SQLite stores repository metadata, files, text chunks, definitions, syntactic
references, imports, reverse import candidates, an ordinary relational path
projection, represented-source response comparisons, and cumulative observed
service accounting. External-content
FTS5 tables provide word and trigram indexes over chunks.

Savings data uses additive tables and file columns without advancing the core
cache schema version. Older LeanToken releases ignore those fields and can
still open or rebuild the cache; the current release repopulates exact
whole-file token metadata on its next reconciliation.

Successful retrieval accounting has one row per tokenizer and each of the
eight fixed retrieval operations. A finalized response performs at most one
best-effort saturating upsert. Exact read `expected_hash` matches add their
not-modified count and represented-source tokens omitted to that same row;
receipt suppression remains a separate counter.

Observed service failures use `service_failures`, keyed by tokenizer, operation,
and a finite, non-sensitive error-variant category. No request source, path,
query, or error message is stored. Its cardinality is bounded by configured
tokenizer names × eight operations × the finite error category set. An
instrumented service boundary performs at most one best-effort saturating
upsert only when the call fails, so successful retrievals do no failure-table
I/O. Evaluation-only APIs outside CLI/MCP are not part of this observation
boundary.

Both accounting writes use a zero-timeout local writer attempt. A busy or
locked writer skips the observation and never delays or fails retrieval. The
combined savings report reads success and failure tables through one pinned
`ReadSession`, preserving request snapshot consistency. Failure reporting is a
primary-key range query on tokenizer, ordered by operation and category; a
checked query-plan test prevents a table scan or temporary sort from entering
this path. These properties make the counters persisted lower bounds rather
than an audit ledger.

LeanToken does not serialize a separate in-memory index snapshot. In this
document, a request snapshot means a SQLite read transaction pinned to one
committed generation. Persisted SQLite generations are disposable derived state
and are reconciled against repository files by the indexing leader.

The connection is configured with:

- WAL journal mode;
- a 16 MiB recycled-WAL size limit (four default SQLite auto-checkpoint
  windows), bounding retained disk after large publications without forcing a
  reader-blocking `TRUNCATE` checkpoint;
- foreign keys;
- a bounded busy timeout;
- bundled SQLite with an FTS5 trigram startup probe;
- transactional schema migrations;
- prepared-statement caching within each request session;
- file/range, reverse-import, and path lookup indexes added through versioned
  migrations so existing databases receive the same query plan as new databases.

Repository-aware service startup binds each database to its canonical
repository root. Default cache paths are already repository-specific; an
explicit database path claimed by a different root is rejected before either
repository can reconcile it. Different repositories therefore have independent
database, lock, watcher, worker, and failure domains. Multiple agents on one
repository intentionally share the same cache and committed generations.

One repository-scoped operation lock serializes reconciliation across processes.
Discovery, hashing, and membership planning happen before publication. An
immediate write transaction then verifies that the generation and config used
to build the plan are still current. A stale plan is discarded and recomputed.
Each file- and byte-bounded Rayon batch is prepared, resolved, and inserted into
that one uncommitted transaction before its memory is released. A later parse,
storage, or cancellation error rolls back every earlier batch. Replacements,
deletions, and generation advancement become visible together at the final
commit.

Profiled reconciliation can additionally attribute relational insertion, each
of the four FTS rebuilds, commit, checkpoint, Linux process write bytes, and
`dbstat` FTS footprints. It also sums per-file worker durations for reads, text
preparation, hashing, parsing, whole-file token counting, chunk token counting,
and record projection. Worker durations overlap and are not wall-clock phases.
This diagnostic path uses a disposable serialized writer connection with
automatic checkpointing disabled and performs an explicit post-commit
`TRUNCATE` checkpoint; the ordinary writer retains normal WAL behavior.
Post-commit diagnostic failure is reported as incomplete profiling rather than
turning an already committed publication into a failed operation. The OpenClaw
profile keeps `columnsize=1`: removing the four FTS docsize tables saved only
1.80% of database bytes and made BM25-backed context roughly 2.5–3.1× slower
because SQLite retokenized external content on demand.

Each multi-step retrieval (search, context, outline, files, read) opens one
checked-out read-only connection from an established, bounded `r2d2_sqlite`
pool and holds a DEFERRED transaction for the request
(`ReadSession`). Under WAL that pins a single committed snapshot for every
query in the assembly, so concurrent publishers cannot mix generations inside
one response. SQLite busy/locked errors while opening and pinning a snapshot
are retried a few times; generation zero returns a typed `IndexNotReady` error
instead of an empty success.

The pool holds at most eight read connections per `Storage` instance. Cloned
services share that pool; separate processes and separate repository caches do
not. This is a concurrency bound, not a promise that eight readers improve
every workload. Change it only with release-mode contention measurements that
include SQLite wait time, end-to-end latency, and memory across the expected
number of simultaneous agents.

Structural search and context assembly pass bounded range/location sets through
SQLite JSON table-valued inputs. SQLite joins hydrate excerpts and enclosing
symbols in batches inside the same request snapshot; LeanToken keeps only the
domain-specific candidate fusion, overlap, and token-selection policy in Rust.

### Storage and policy ownership

The boundary is deliberate:

- SQLite owns indexes, joins, FTS5 search, transactions, relational path
  projection, and keyset pagination.
- `rusqlite` owns prepared-statement caching. Bounded multi-value requests use
  SQLite's `json_each` table-valued input instead of dynamically assembled
  placeholder lists or a local batching framework.
- `r2d2_sqlite` owns connection pooling. The application does not implement a
  second cache or pool above it.
- The indexer owns language-specific import candidate generation because those
  candidates are product policy, then stores them in indexed relational tables
  for resolution and reverse invalidation.
- Ranking owns evidence fusion, overlap-aware deterministic deduplication, and
  token-budget selection. These semantics are observable retrieval behavior and
  are not delegated to the storage engine.
- Reconciliation owns explicit change classification and generation-checked
  publication. SQLite supplies atomicity; the application decides what a
  repository change means.

New hot-path code should first express data access as a bounded storage query.
Add a custom data structure only after a release-mode profile identifies a
remaining bottleneck and the replacement preserves snapshot, ordering, and
limit semantics.

Index-backed MCP and CLI retrievals expose an explicit consistency boundary.
For long-lived MCP clients, `indexed_generation` is the default and opens the
latest completed snapshot immediately without implying Git HEAD. One-shot CLI
retrievals default to `reconcile_working_tree`; callers can explicitly select
`indexed_generation` when snapshot latency is more important than scanning live
changes. `reconcile_working_tree` first runs a non-rebuild reconciliation under
the repository-scoped operation lock, then opens the resulting completed
snapshot. This makes filesystem changes completed before reconciliation visible
without exposing a partially prepared generation. Changes written concurrently
may require a later request.

Clones of one `Services` instance share a reconciliation coordinator. Requests
may join the current wave until it acquires the repository operation lock and
marks its scan started. Requests arriving after that boundary join one pending
wave, which starts after the current wave finishes. This removes redundant
serialized scans without allowing a later caller to inherit a scan that may
have started before its edits. Cancelling or aborting one waiter detaches only
that waiter. A started wave remains service-owned; an unused pending wave is
cancelled before it starts. A failed wave shares its original typed error with
every remaining waiter; it does not submit one retry scan per coalesced caller.
Waiter admission is fail-fast and bounded to the
same 16 active requests as the Services blocking executor, so direct library
callers cannot create an unbounded pending-waiter collection.

## Indexing and freshness

Status keeps committed-index readiness orthogonal to reconciliation activity.
Generation zero is `index_state: "uninitialized"`; every later generation is
`index_state: "ready"`. Independently, an idle cache is
`freshness: "current"` and an active local or cross-process reconciliation is
`freshness: "reconciling"`. The observable combinations are therefore
`uninitialized/current` before indexing, `uninitialized/reconciling` during the
first build, `ready/current` after a generation commits, and
`ready/reconciling` while replacing an existing generation. No failed state is
reported because reconciliation failures are not persisted. Status itself does
not scan the working tree and therefore reports `working_tree_checked: false`;
its freshness value describes reconciliation activity only.

Discovery follows Git-compatible ignore rules, skips symlinks and oversized or
binary files, and normalizes indexed paths to forward slashes. Text files are
hashed, chunked on UTF-8 boundaries, and parsed in a bounded Rayon pool.
The ignore-aware walker counts every yielded file, directory, and error entry,
then separately counts admitted files and their aggregate metadata bytes. It
fails on the first configured entry, file, byte, or depth limit violation rather
than returning partial membership. Preparation scheduling is additionally
bounded by file and byte batch limits. All discovery limits participate in the
index configuration hash, so changing them forces a complete atomic
reconciliation before the new policy is recorded.

One repository-owned discovery policy configures full walks, visibility
fallbacks, and watcher intake. It retains hidden source/configuration paths,
loads nested `.leantokenignore` files above `.gitignore` and `.ignore` in rule
precedence, and prunes a conservative set of generated and package-cache
directories before descending. The explicit include-generated setting disables
only that built-in pruning and participates in the index configuration hash.
Watcher callbacks apply the same built-in policy before enqueueing raw events,
while ignore-control changes remain visible and trigger bounded full discovery.
Recursive-watcher admission examines at most 100,000 total filesystem entries
while proving the 50,000-directory registration bound. The admission walk runs
as cancellable blocking work; entry overflow, cancellation, or traversal error
selects periodic polling instead of delaying the async runtime.

Configured SQLite databases and their four coordination lock sidecars are always
excluded from repository membership. Old unconfigured coordination sidecars are
excluded only when they are zero-byte regular files named exactly
`index.sqlite.{lease,init,leader,index}.lock`; arbitrary `.lock` files and
non-empty same-name files remain indexable. Full discovery, visibility
fallbacks, and targeted watcher reconciliation use the same predicate, and a
targeted event removes a previously indexed file when it becomes a recognized
sidecar. Recognition adds no independent filesystem walk and performs metadata
inspection only after the exact filename shape matches.

Canonical filesystem roots, the current user's home directory, and ancestors of
that home directory are rejected before cache or watcher initialization unless
the caller explicitly opts into broad-root indexing. MCP performs this check
after the protocol initialize exchange so a bad host working directory fails
closed without recreating the startup handshake timeout.

MCP starts the stdio protocol before opening SQLite or indexing. It answers the
mandatory initialize exchange first, then starts repository services after the
client's initialized notification. A generation-zero retrieval waits up to 30
seconds for the first publication and release of the repository operation lock,
with caller cancellation, before running the retrieval once more. It does not
poll by repeatedly executing the retrieval. If the bound expires, it returns a
successful structured `status: "retryable"` result rather than a tool error or
an empty retrieval result. This keeps short cold-index waits inside one tool call
instead of requiring another model turn. An existing complete generation remains
queryable while its replacement is prepared.

A generation-zero cache with neither a local reconciliation nor a held
cross-process operation lock gets a one-second leadership grace rather than the
full wait. If no owner appears, the call returns the same retry guidance so a
terminally failed leader or delayed failover cannot consume 30 seconds on every
follower request.

Cache initialization, schema migration, and managed-cache corruption recovery
run under a separate repository-scoped initialization lock. SQLite busy and
locked results are retried with bounded backoff and caller-owned cancellation;
terminal startup failures move MCP tools to an unavailable state. The stdio
adapter supervises the indexing runtime for the lifetime of the connection, so
an unexpected runtime exit cannot leave tools permanently reporting startup.
Index limit violations are terminal configuration failures: the leader shuts
down its watcher, releases leadership, and moves MCP tools to unavailable
without periodic retries. A restart with a narrower root or adjusted limits is
required.

Schema v5 records a Unix last-access timestamp when a repository is bound during
service startup; retrieval calls do not turn every read into a metadata write.
Central cache inspection opens SQLite read-only and falls back to direct artifact
mtime for corrupt, incomplete, or older-schema entries.

Cache metadata/access state and index-content compatibility are separate
classifications. A readable current metadata schema can therefore coexist with
an `obsolete_older` or `legacy_unversioned` content identity without being
reported as content-compatible. Versioned list requests accept at most five
compatibility classes and 32 exact content versions; both filters and the
`incompatible_with_current` convenience predicate are included in the cursor
shape.

Every service instance acquires a shared cache lease before initialization and
keeps it through all clones. Explicit pruning requires the exclusive lease, so
active leaders and read-only followers are both protected rather than relying on
the shorter leadership or operation locks. The lease identity remains after
large cache artifacts and coordination sidecars are removed; replacing or
unlinking the lock itself would let a returning process lock a different inode.
Only strict legacy hashes or `v<index-content-version>-<hash>` directories under
the platform-managed cache root participate; unexpected directory content,
future content versions, and explicit databases outside that root fail closed
from automatic deletion. Versioned identities let compatible builds share a
cache without allowing an older process left alive during an upgrade to
downgrade the newer index. Compatibility pruning deletes only inactive,
recognizable older or legacy-unversioned entries and re-inspects the same
criterion after acquiring the exclusive lease. Corrupt/unknown, future,
unexpected, identity-mismatched, and lease-unavailable entries are never
automatically deleted.

An explicit database path preserves the caller-selected identity and is not
rewritten by the managed-cache policy. Callers must not concurrently share one
explicit database across incompatible index-content versions.

MCP processes sharing one cache compete for a repository-scoped leadership
lock. The leader alone owns automatic indexing and one filesystem watcher;
followers normally read the same committed SQLite generations without scanning
or watching. An explicit `reconcile_working_tree` retrieval may reconcile from
any process under the shared operation lock. Followers retry leadership with
capped exponential polling from 500 milliseconds through eight seconds, so
stable followers stop opening the lock file twice per second while an
operating-system lock release after process exit still provides bounded
failover without a PID lease or stale-lock cleanup.

The leader registers its watcher before the initial reconciliation, preserving
the startup event-gap guarantee. The automatic-indexing runtime uses a
single-slot public queue; raw events, retained paths, and incomplete rename
cookies have separate hard bounds. Overflow or ambiguity discards detailed
path state in favor of one sticky full-reconciliation request, so a long initial
scan cannot accumulate an unbounded event backlog.

After any scan, queued messages drain into one bounded scheduler state. Path
changes deduplicate and wait for the configured quiet period. Ambiguous rename
sequences, backend rescan requests, public queue overflow, or scheduler path
overflow upgrade that state to one full reconciliation. Consecutive full scans
use a capped exponential cooldown, while transient reconciliation failures
retain the same pending work under a separate capped exponential retry. Root,
limit, repository-binding, and configuration failures are terminal and stop
the indexing runtime instead of entering either retry loop.

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
instance's `max_index_workers`. MCP background indexing defaults to one worker
so protocol handling and sibling agents retain CPU capacity; an explicit
`--max-index-workers` value is preserved. Direct `index` commands retain the
normal bounded default. The pool is built lazily on the first non-empty
file preparation and reused afterward. Read-only followers therefore allocate
no indexing threads, while a process that becomes leader retains its configured
worker bound without rebuilding a pool on every reconciliation.

Request result, token, and context-line bounds are validated in `Services`, so
library and direct MCP callers receive the same contract as the CLI. CLI
positive-integer parsers and MCP JSON Schema ranges provide earlier feedback
but are not treated as enforcement boundaries. MCP startup, ready, and failed
states retain one validated configured-limit snapshot, so readiness does not
change whether an explicit value is accepted. Zero is valid only for
`context_lines`; values above an active maximum return a structured
`RequestLimitExceeded` error rather than being clamped.

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
| Focus patterns with local candidate generation | 32 |
| Focused indexed files inspected per pattern | First 4 policy-eligible paths in lexical order |
| File-local focused records inspected | 256 chunks and 128 symbols per file |
| Focus-local candidates retained per pattern | 8 |
| Focus-local storage lookups | At most 256 (32 patterns × 4 files × 2 record kinds) |
| Regex matching chunks | `min(max_results × 20, 2000)` |
| Trigram candidate chunks | 10000 |
| Lightweight rows inspected for path-scoped trigram planning | 100000 |
| Full-scan fallback files | 10000 |
| Full-scan fallback chunks per file | 256 |
| File scan page size | 1000 for find/glob; tree queries `max_results + 1` projected paths |
| Opt-in compact projection materialization | At most the 100 selected files, symbols, groups, or hits already admitted by `max_results`; no additional repository scan |
| Opt-in response-bounded read materializations | At most 18 within one pinned generation |
| Batched history targets / page | 64 requested / 32 returned |
| Batched history distinct paths | 32 per revision endpoint |
| Batched history blob bytes | 1 MiB per file, 8 MiB per revision endpoint |
| Batched history parsed symbols | 1,024 per revision endpoint (2,048 total) |
| Batched history retained diff | 1 MiB per response page |
| Batched history Git subprocesses | At most 7, independent of target count |

Focus quotas do not depend on global per-query top-N channels. During the
existing 512-row paged constraint scan, context counts every indexed focus
match and retains the first four policy-eligible paths in lexical order. Before
ranking it inspects bounded file-local chunks and symbols, prefers task-matching
structural and lexical excerpts, and uses a deterministically task-scored chunk
only when the focused files contain no semantic hit. At most eight candidates
per pattern enter global deduplication and quota reservation.

Requests above 32 focus patterns or a per-pattern minimum above eight fail with
a typed limit error. A broad pattern resolving beyond four eligible files emits
an explicit incomplete warning. Include/exclude, generated-artifact, strict
focus, and strict changed-path policies apply before local generation, and a
policy-empty focus scope is reported separately from a pattern that matches no
indexed file.

The 12-query context planner retains up to four early domain terms and two
high-specificity terms selected from the remainder of the complete task.
Natural-language tasks retain at most two deterministic bigrams; tasks with
technical atoms retain one bigram while reserving up to four exact-atom slots.
This reduces sentence-order sensitivity without making query fan-out depend on
task length.

Regex mode first parses a bounded HIR candidate plan. Mandatory case-sensitive
ASCII word literals of at least three bytes become trigram `AND`/`OR`
expressions; the compiled Rust regex then verifies only those candidate chunks.
Alternations with an unplanned branch, optional-only literals,
case-insensitive Unicode semantics, short literals, and planner budget
exhaustion retain the bounded full-scan fallback. A capped FTS row-count
preflight rejects plans with more than 10,000 candidate chunks without loading
their bodies. Sound plans do not inherit the fallback's 10,000-file or
256-chunk-per-file scan bounds because they do not enumerate those rows.
Path-scoped plans apply include/exclude filters to lightweight chunk ID/path
rows before the 10,000-candidate bound, hydrate only admitted IDs, and fail
explicitly if more than 100,000 FTS rows would need inspection. Both paths
retain the matching-chunk limit, while the fallback retains its file and
per-file chunk limits. Compiled regex size and DFA cache are also limited so
pathological patterns fail closed.

Every retrieval operation accepts an optional serialized service-response
ceiling through `ServiceCallOptions`, MCP `max_response_tokens`, or CLI
`--max-response-tokens`. This boundary counts the final compact service DTO,
including paths, diagnostics, receipts, metadata, and the accounting fields
themselves. It does not count MCP `CallToolResult` duplication, JSON-RPC
framing, or human CLI rendering. `token_budget`/`--budget` remains the
independent source-content ceiling.

Fitting is deterministic and happens inside `Services`, after candidate
generation but before receipt evidence is committed. It first removes bounded
omission facets, detailed diff evidence, routing detail, and ranking reasons.
Only requests without include, must-cover, focus, diff, strict-scope, or
handoff constraints may then drop lowest-ranked selected fragments. Constrained
requests return a typed `RequestLimitExceeded` error when their correctness
skeleton cannot fit; fitting never weakens their coverage contract. Default
plan-only diff context omits detailed diff evidence unless
`verbose_diagnostics` is requested. Receipt sizing reserves the exact
request/generated receipt identifier plus conservative counter and warning
shapes, and the final postcondition is checked after receipt application.
Accounting converges to a fixed point and
`meta.total_response_tokens` is the exact inclusive serialized DTO count.
The shared `ResponseBudget` counter provides a logarithmic largest-prefix
primitive; operation services supply the response-shaped projection and
correctness skeleton rather than applying generic JSON truncation.

Path discovery derives continuation cursors from the last retained path (and
fuzzy score), history truncates only UTF-8 character boundaries or commit
prefixes, and JSON keys pagination binds a reduced page to the same source and
query hashes. Search and outline validate a conservative receipt shape before
committing receipt evidence. If their already source-bounded page does not fit,
they return a typed limit error instead of manufacturing a cursor that could
skip omitted evidence. Fixed-shape JSON projections use the same fail-loud
rule; shallow schema degradation remains owned by the JSON projection stage.

Batched symbol history resolves both revisions once, reads commit metadata in
one command, performs one tree lookup per endpoint, and uses at most one
`cat-file` batch per endpoint. Each distinct requested path is parsed once and
reused for all targets on that page. Endpoint parsing quotas are independent so
a symbol-heavy base cannot starve head evidence. The cursor hashes resolved
base/head commits and ordered normalized target pairings. Response fitting
retains the largest ordered status skeleton before spending remaining capacity
on diff prefixes; it never keeps a large early diff at the cost of otherwise
representable symbol outcomes.

Compact response projections are explicit and never replace the default DTO.
`files=paths` maps the already selected entry page to ordered strings and keeps
the same keyset cursor. `search=grouped` groups at most the selected search page,
retains only one source excerpt per group, and summarizes reference hits without
another index lookup. `outline=signatures` excludes imports during the existing
bounded outline walk, drops byte offsets, and hashes each file's ordered compact
signature array once. Its cursor query hash includes the projection so full and
signature-only offsets cannot be mixed. All three projections are finalized and
checked against `max_response_tokens` inside `Services`; a compact correctness
skeleton that cannot fit returns typed `RequestLimitExceeded`.

MCP JSON keys use depth-then-pointer order and an optional maximum depth of 64;
root depth is zero and array elements share one wildcard path. Version-two keys
cursors bind this traversal shape, while the public Rust service keeps its
legacy pointer ordering and version-one cursor contract. Schema fitting builds
at most 10,000 in-memory nodes per projection and performs at most 16
deterministic breadth-first materializations (the item-limited candidate, root,
and logarithmic token fitting). Incomplete schema metadata returns at most 32
sorted omission-frontier pointers plus the exact frontier count. These passes
operate on the already byte-bounded parsed JSON value and perform no filesystem
rescan.

When an explicitly response-bounded `read` page does not initially fit, the
service binary-searches the existing source-token ceiling and rematerializes at
most 18 bounded live pages inside the same SQLite generation. Each probe keeps
the existing 8 MiB live-read cap and cancellation checks. The selected page
uses the normal content-bound continuation cursor, so fitting cannot skip
source. Delta mode falls back to that fitted direct page if delta metadata
would cross the total-response ceiling.

Run the reproducible hot-path profile with, for example,
`cargo run --example hot_path_bounds --release -- --files 10000 --iterations 20`.
It reports warm p50/p95 wall time plus deterministic regex and context phase
counters. Evaluation-only context output also includes diagnostic wall-time
phases; candidate generation includes its nested lookup phases, so those fields
must not be summed. Use counters—not timing thresholds—to assert candidate
selection and fallback behavior. Run the command under `/usr/bin/time -v` when
process CPU and peak RSS are required. Results are host-local and should only
be compared on the same machine and release profile.

`real_repository_profile` applies the same counters to an existing checkout
while keeping its SQLite index outside the source tree. Omit `--database` for a
disposable index, or provide it and use `--skip-index` for subsequent
steady-state samples:

```bash
cargo run --release --example real_repository_profile -- \
  --repository /path/to/repository --iterations 5
```

## Retrieval admission and execution

MCP dispatch, protocol admission, and blocking execution are separate
ownership boundaries. The bounded stdio transport admits at most 16 decoded
`tools/call` requests into rmcp. It retains each permit by JSON-RPC request ID
until the corresponding response write finishes, covering both handler work
and a response waiting on rmcp's bounded sink. A request-extension guard
releases the permit if the handler unwinds before producing a response. Excess
calls receive the same fail-fast retryable capacity response before rmcp can
create another task. Initialization and `tools/list` bypass this dispatch gate.

Inside the handler, each `LeanTokenMcp` server independently admits at most 16
active `tools/call` requests after repository identity validation. Clones of
that server share both process-local governors; a separately constructed server
or another MCP process has independent capacity. Handler admission covers all
eight tools, including `savings`, but does not cover initialization or
`tools/list`. The transport gate bounds protocol task memory; handler admission
keeps repository identity checking ahead of execution-facing capacity.

`Services` owns a generic blocking executor for retrieval, status, savings, and
evaluation calls. Its current safety defaults allow 16 active operations: at
most eight blocking closures may run and at most eight more may wait for an
execution permit. Further work fails immediately with
`RetrievalOverloaded`; admitted work that waits 500 ms without obtaining an
execution permit returns `RetrievalQueueTimeout`. These capacities are
configured independently from MCP admission even though both active defaults
are currently 16.

Execution permits move into the blocking closure. Aborting the async caller
therefore does not release capacity while its closure is still running.
Cancellation or timeout while waiting drops the active permit without starting
a closure, and normal completion, error, cancellation, or panic returns both
permits. Indexing remains on its existing reconciliation path so retrieval
backpressure cannot acquire or release indexing ownership.

The execution default matches the eight-connection SQLite reader pool. This is
a bounded safety choice, not a claim that eight is universally optimal.
Release-mode measurements must justify changing either default. A read request
checks out one pooled connection and holds one DEFERRED WAL transaction for its
consistent generation. That snapshot is transaction state, not a copied
database or per-request artifact. Its main storage effect is that SQLite may
retain old WAL pages until the reader finishes; dropping the request session
rolls back the read transaction and returns the connection.

## Live read vs index

`leantoken.read` always reads the live filesystem for the returned body while
symbol resolution and path admission use the index. A complete read hashes the
file and extracts its bounded range during one forward stream. A
token-truncated read performs one additional full-hash verification before
issuing a continuation cursor. Responses include:

- `meta.repository_generation` — committed generation used for index lookups;
- `meta.freshness` — `current` or `reconciling` (local activity or the shared
  operation lock);
- `content_hash` — hash of the returned live range;
- `indexed_hash` — hash of the whole indexed file;
- `index_stale` — true when the live file body differs from the indexed file.

When `index_stale` is true, agents should re-outline or re-search with
`consistency=reconcile_working_tree` if the next retrieval must include those
edits. Pass `expected_hash` on rereads to suppress unchanged ranges. Exact
line, symbol, and heading reads can opt into a repository-local bounded delta
registry. It retains only complete targets and returns a unified diff only when
the target coordinates still match and the complete delta is strictly cheaper
than current content. Search and outline never invent empty successful results
at generation zero.

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
- MCP request admission and Services blocking execution are instance-local
  within one process. Another workspace or agent affects these limits only
  when it shares that same server or `Services` instance; a separate MCP
  process has separate limits.
- Reconciliation waves are also instance-local. Cloned Services share wave
  state, while independently opened Services and separate processes continue
  to serialize through the repository operation lock. Explicit CLI indexing,
  watcher path reconciliation, and rebuilds remain outside wave coalescing.
- Stdio input frames are bounded at four MiB while bytes are read. Oversized
  terminated or unterminated frames are discarded without retaining their
  capacity, after which the next newline-delimited request can proceed.
- Retrieval never exposes a partially built generation, and generation zero is
  never rendered as a successful empty repository.
- Automatic work does not delay the MCP initialize response, and startup does
  not invent unsolicited MCP progress tokens.

## Parsing

Tree-sitter extracts syntax facts for Rust, Python, JavaScript, TypeScript/TSX,
Go, C/C++, C#, Java, PHP, Ruby, HTML, and CSS. LeanToken stores flat
definitions, syntactic references, signatures, parents, and imports; syntax
trees are discarded after indexing.

JavaScript-family extraction supplements upstream tags with program-level data
bindings and class fields. It deliberately excludes function-local variables,
while retaining complete declarator ranges so outline, symbol search, and
symbol read can navigate large object and array literals.

HTML and CSS use grammar-specific structural extraction over the same
tree-sitter parse. CSS selector and at-rule symbols retain complete rule ranges;
HTML ID and element symbols retain complete owning-element ranges. Attribute
and selector references keep their exact lexical ranges, while resource links
flow through the shared import model.

C# extraction covers namespaces, classes, structs, interfaces, records, enums,
delegates, methods, local functions, constructors, properties, fields, events,
indexers, and operators. Method-like symbols retain their complete bodies,
file-scoped namespaces own the declarations that follow them, and type,
invocation, and object creation references remain associated with their
enclosing symbols.

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

An opt-in handoff manifest is assembled inside the same pinned context
generation. It captures selected coordinates and hashes before server-receipt
suppression, then derives bounded changed, related, and likely test paths from
the completed diff receipt. No source body enters the manifest. Git probes are
time-bounded; unavailable commit or working-tree provenance becomes an explicit
gap instead of a clean-state guess. The final response is token-accounted only
after the manifest is attached, so protocol cost remains visible.

Immutable review context can derive a model-free semantic change receipt after
ranking. The repository layer resolves each revision once, maps the bounded path
set with `git ls-tree`, and reads selected unique objects with one
`git cat-file --batch` call per side. Per-file and 8 MiB aggregate byte limits
apply before parsing. Classification uses parser signatures, exact symbol
identity, and unique normalized body fingerprints; ambiguous overloads or
renames are omitted with gaps. Recognized JSON configuration is compared through
canonical value fingerprints, but only RFC 6901 key paths and change kinds leave
the service.

Each service instance owns its tokenizer configuration. Exact OpenAI BPE
encodings use `tiktoken-rs` singleton vocabularies; the explicit estimate mode
is marked inexact. Protocol-cost benchmarks serialize the actual tool catalog,
JSON-RPC requests and responses, result wrappers, and repeated-context handoff
instead of adding a guessed fixed overhead.

## Path and data safety

All repository-facing paths are relative. Absolute paths, parent traversal,
NUL bytes, and canonical paths outside the repository root are rejected.
Symlink escapes are rejected when live content is opened. `leantoken.read`
requires an indexed path, so ignore rules also govern which files can be read
through the tool.

LeanToken is read-only with respect to repository source. It does not execute
project commands or make network requests. Context ranking may invoke a bounded
`git status` process for an optional working-tree signal; timeout, producer-side
byte overflow, or failure removes that signal. Revision, history, name-only,
blob, and diff-hunk probes likewise read through bounded pipes. Git runs in a
platform process group so timeout, output overflow, or an outliving helper
terminates the whole descendant group before the reader is joined; partial
output is never accepted. SQL values are parameterized. Logs contain
paths, counts, hashes, timings, and error summaries but not source bodies by
default; dependency events carrying complete MCP `request` or `result` fields
are filtered even when debug tracing is enabled.

Setup journals and configuration replacements sync file contents before atomic
publication. On Unix, each publish or removal then syncs the containing
directory, including journal creation and commit, so recovery ordering covers
power loss as well as process interruption. Other platforms retain file-sync
and atomic-replacement guarantees where `std::fs` exposes no directory-sync
contract.

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
- Optional semantic classification fails open into bounded coverage gaps for
  unreadable, oversized, unsupported, non-UTF-8, or structurally incomplete
  historical files; it does not fail an otherwise valid context response.
- Committed WAL state survives process failure. Confirmed corruption in a
  LeanToken-owned cache is deleted and rebuilt; an explicitly configured
  caller-owned database is preserved and the error is returned.
- EOF and orderly cancellation stop stdio service, watcher, and reconciliation
  tasks without detached worker threads. If the leader exits, a follower takes
  ownership and reconciles before resuming automatic watching.
