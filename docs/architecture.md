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
service accounting. It also stores bounded retrieval receipt headers and
evidence metadata for cross-process suppression. External-content
FTS5 tables provide word and trigram indexes over chunks.

Savings data uses additive tables and file columns without advancing the core
cache schema version. Older LeanToken releases ignore those fields and can
still open or rebuild the cache; the current release repopulates exact
whole-file token metadata on its next reconciliation.

Successful retrieval accounting has one row per tokenizer and each of the
eight fixed retrieval operations. A finalized response performs at most one
best-effort saturating upsert. Exact read `expected_hash` matches add their
not-modified count and represented-source tokens omitted to that same row;
receipt suppression remains a separate counter. Four additive counters classify
new successful responses as useful, incomplete, unsupported, or
hash-suppressed; failures remain in the failure table. Only useful responses
with a represented-source baseline update the effective source-compression
columns. Full-response accounting still includes every successful class.

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
this path. The successful-accounting read has matching query-plan evidence for
the `(tokenizer, operation)` primary key.

Savings deltas do not add stored events. A response serializes the pinned
aggregate counters into a compact, checksummed, caller-carried opaque snapshot
bound to repository identity and tokenizer. A later request subtracts that
snapshot with checked arithmetic and fails closed on malformed, cross-repository,
cross-tokenizer, reset, or future counters. Snapshot input and output are each
bounded to 32 KiB; current rows remain bounded by eight operations and failures
by the finite category matrix. These properties make the counters persisted
lower bounds rather than an audit ledger.

Retrieval receipts persist evidence metadata, never task/query text or raw
source. An opaque ID combines a random 128-bit database-incarnation namespace
with a SQLite `AUTOINCREMENT` row, so concurrent processes, cache recreation,
and different repository databases cannot reuse an ID. Each evaluate loads the
requested header and at most 2,048 ordered evidence rows, computes the same
exact/overlap/near-duplicate decisions as the former process-local oracle, and
appends only returned evidence. Header lookup, evidence lookup, expiry pruning,
and LRU selection have checked primary-key/range-index query plans.

Receipt evaluation uses one `IMMEDIATE` transaction for lookup, generation and
clock validation, decisions, append, counters, expiry, and quota eviction.
The process-local writer mutex and SQLite's database writer lock therefore make
two processes using one receipt observe a serial order: a duplicate concurrent
call is returned once and suppressed by the follower, and distinct appends
cannot lose one another. A receipt remains bound to the generation of the read
snapshot that produced it even if a newer generation publishes before the
receipt transaction. A later request on the new generation fails with
`StaleReceipt`; it never silently creates a new session.

The hard receipt bounds are 128 headers, 64 KiB of logical header data, 2,048
evidence rows and 1 MiB of logical evidence per receipt, and 16,384 evidence
rows or 8 MiB of logical evidence globally. Logical bytes include persisted
field values and fixed-width scalar fields, not SQLite page overhead. A
monotonic database access sequence makes LRU ties deterministic. Appending new
evidence refreshes access immediately; an access that appends nothing refreshes
LRU and the sliding 24-hour wall-clock expiry at most once per 60 seconds,
bounding write amplification without allowing an actively reused receipt to
expire. Expiry and observed clock rollback fail closed. Lazy pruning runs
inside receipt evaluation. Capacity eviction and expiry deliberately become
`UnknownReceipt`, while generation-stale headers remain available until expiry
or capacity pressure so callers normally receive the more specific
`StaleReceipt`. Whole-cache prune removes receipts with the same disposable
database. One operation can inspect at most 128 headers and 16,384 cascading
evidence rows during worst-case quota cleanup; there is no filesystem scan,
subprocess, network access, or concurrency fan-out.

Opt-in read deltas persist a narrower safe subset of their bases in the same
repository SQLite cache. A base is eligible only when the returned target is
complete, at most 512 KiB, and the complete live file hash equals the indexed
file hash from the same pinned repository generation. Dirty or otherwise
live-diverged content remains process-local. Truncated and oversized targets
are not persisted, and ignored or unindexed paths fail before delta capture.
The response receipt reports whether the selected base came from process-local
or persistent state, whether the current head was persisted, and the bounded
fallback reason. Persisted rows contain the exact eligible target content;
computed unified deltas remain process-local because they are cheap to
recompute and can contain removed lines.

The persistent target key hashes repository identity, normalized path, target
kind, and target selector. Raw paths and request text are not duplicated into
the delta table. The content stays in the existing index database, so it has
the same file ownership, permissions, cache lifecycle, repository binding, and
whole-cache prune behavior as indexed chunks; no additional source-bearing
sidecar is created. Content hashes are verified on load and duplicate insert,
and clock rollback or corruption fails closed.

Persistent read-delta state retains at most 128 bases, 512 KiB per base, and
8 MiB of logical base data in total, with a sliding 30-minute wall-clock TTL.
An access sequence gives deterministic LRU eviction; automatic base selection
orders by newest repository generation independently of LRU recency. Exact
unchanged metadata refreshes are debounced for 60 seconds. Lookup, lazy expiry,
refresh, insertion, and quota eviction use one `IMMEDIATE` transaction, so
independent processes observe a serial writer order without lost updates. One
operation can evict at most the 128 retained rows and performs no filesystem
scan, subprocess, network access, or concurrency fan-out. Checked query plans
use the `(target_key, content_hash)` primary key, target/generation index,
expiry index, and LRU index; the LRU plan scans only the covering index to its
first row.

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

Cooperative cancellation is checked between each FTS publication phase and
immediately before commit. Cancellation observed at one of those boundaries
rolls the transaction back; an individual SQLite FTS statement remains the
smallest non-interruptible unit. Once commit returns successfully, that
generation is authoritative and the caller receives committed success even if
its cancellation token changes afterward. A post-commit cancellation can stop
later work, but cannot retroactively turn a visible generation into a failed
reconciliation outcome.

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
precedence, and prunes `.git` metadata plus a conservative set of generated and
package-cache directories before descending. The explicit include-generated
setting disables only generated-tree pruning; `.git` remains excluded, and the
setting participates in the index configuration hash. Watcher callbacks apply
the same built-in policy before enqueueing raw events, while ignore-control
changes remain visible and trigger bounded full discovery.
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

MCP retrieval preparation gives generation-zero `reconcile_working_tree`
waiters the same absolute 30-second cold-index deadline as readiness waiting.
The service probes the committed generation before applying that deadline, so
reconciliation against an existing generation retains its historical behavior.
If a cold waiter reaches the deadline, it is removed from the coalesced wave;
an otherwise empty wave waiting for the operation lock is cancelled before it
can scan. The adapter returns the existing `index_building` structured retry
instead of reading generation zero or waiting for the host timeout. CLI and
library calls do not set this MCP-owned deadline.

Generation-zero full reconciliation also maintains one process-local, bounded
progress snapshot. The snapshot contains only a fixed phase enum, timestamps,
opaque 128-bit cache and attempt identities, a monotonic update sequence, and
aggregate discovery/preparation/staging counters; it never retains repository
paths, source, queries, or warning lists. The cache namespace hashes the
index-content version and database identity instead of exposing that path.
Discovery updates the snapshot once per 256 walker entries and once at
completion. Preparation updates once per bounded batch, and publication updates
only at relational, FTS, and commit boundaries, so progress reporting adds
neither per-file locking nor SQLite writes. Readers copy the small snapshot
under its own mutex and never acquire the operation or writer lock.

The benchmark-only dependency-heavy cold-index lane reads that same bounded
snapshot to attribute sampled process resources to phases. Each matrix arm uses
one fresh process and one fresh database; no more than 16 arms or 64 preparation
workers can be requested. The parent launches arms serially, retains no source
content, and kills a child that exceeds its cooperative timeout plus the
ten-minute cancellation grace and a fixed launch allowance. Within a child,
resource sampling is bounded to one observation every 1–1,000 milliseconds and
reads only `/proc/self/{stat,status,io}` plus metadata for the configured SQLite
main, WAL, and SHM files. At most 16 retrieval-parity queries of 256 bytes each
are replayed. This profiler-only fan-out does not alter MCP worker policy,
discovery membership, preparation batch bounds, publication atomicity, or the
retrieval hot path.

The process performing the initial index exposes detailed phases for discovery,
hash-and-plan, preparation, relational staging, the four FTS builds, and
commit/checkpoint. `files_staged` remains explicitly unpublished until the
ordinary atomic transaction commits; progress observation does not create
partial generations. SQLite's ordinary auto-checkpoint can run inside commit,
so the final non-terminal phase is honestly named `commit_and_checkpoint`
rather than inventing a separately observable checkpoint. Every retry attempt
replaces the prior counters and identity. Terminal guard updates are accepted
only for their matching attempt, preventing cancellation, failure, takeover, or
stale guard destruction from making an older attempt current.

This snapshot is intentionally not persisted or shared through another
sidecar. A same-process MCP leader can include full details in `index_building`
and status responses. A follower reports `detail_available: false`, the
committed generation it actually observed, and whether the nonblocking
coordination probe sees an active reconciliation; optional counters remain
absent rather than invented as zero. Completed generations omit
`index_progress`. A worst-case fixed-shape detailed retry payload is regression
tested at no more than 256 `cl100k_base` tokens.

The leader registers its watcher before the initial reconciliation, preserving
the startup event-gap guarantee. The automatic-indexing runtime uses a
single-slot public queue; raw events, retained paths, and incomplete rename
cookies have separate hard bounds. Overflow or ambiguity discards detailed
path state in favor of one sticky full-reconciliation request, so a long initial
scan cannot accumulate an unbounded event backlog.

Watcher initialization exposes a bounded diagnostic snapshot containing the
selected native or periodic-polling backend, the exact admission entries and
directories examined, the fallback reason, and atomic poll/path/full-delivery
counters. A polling fallback schedules its first full reconciliation only after
the 30-second interval. It never emits an immediate poll after the mandatory
startup reconciliation; subsequent missed ticks are skipped rather than
replayed in a burst.

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
when a scan boundary is reached. Tree and glob pages use the indexed
`path_entries` projection with a path keyset cursor; glob filters file rows with
SQLite `GLOB` (patterns that cannot map, such as brace expansion, fall back to a
bounded globset scan). Find still scans indexed files with a lean path-only
projection (`id`, `path`, `language`, `size_bytes`) because fuzzy nucleo scoring
does not map to SQL. The numbers are safety limits, not monorepo performance
claims.

| Path | Bound |
| --- | --- |
| Context query terms | 12 (`MAX_CONTEXT_QUERIES`) |
| Workflow-evidence items | 8 per class, 8 KiB per item, 32 KiB total |
| Context hits per term/source | 20 symbols/refs, 30 FTS |
| Focus patterns with local candidate generation | 32 |
| Focused indexed files inspected per pattern | First 4 policy-eligible paths in lexical order |
| File-local focused records inspected | 256 chunks and 128 symbols per file |
| Focus-local candidates retained per pattern | 8 |
| Focus-local storage lookups | At most 256 (32 patterns × 4 files × 2 record kinds) |
| Must-path task-relevance inspection | First eligible file and 256 chunks per pattern, at most 256 patterns |
| Required-evidence contracts | 32 contracts, 16 literal queries each, 64 KiB query text total |
| Required-evidence local inspection | First 4 policy-eligible paths and 256 chunks per path/contract |
| Required-evidence candidates | 8 centered excerpts per contract, 40 lines per excerpt |
| Regex matching chunks | `min(max_results × 20, 2000)` |
| Trigram candidate chunks | 10000 |
| Lightweight rows inspected for path-scoped trigram planning | 100000 |
| Full-scan fallback files | 10000 |
| Full-scan fallback chunks per file | 256 |
| File scan page size | 1000 for find (path projection) and globset fallback; tree/glob SQL-page `max_results + 1` projected paths |
| Opt-in compact projection materialization | At most the 100 selected files, symbols, groups, or hits already admitted by `max_results`; no additional repository scan |
| Exhaustive occurrence grouping | At most 100 selected occurrence coordinates and 100 group-map entries per response page; the existing 100,000-occurrence fail-closed scan cap is unchanged |
| Opt-in response-bounded read materializations | At most 18 within one pinned generation |
| Batched history targets / page | 64 requested / 32 returned |
| Batched history distinct paths | 32 per revision endpoint |
| Batched history blob bytes | 1 MiB per file, 8 MiB per revision endpoint |
| Batched history parsed symbols | 1,024 per revision endpoint (2,048 total) |
| Batched history retained diff | 1 MiB per response page |
| Batched history Git subprocesses | At most 7, independent of target count |
| Offline context-utilization artifacts | 64 MiB each, 100,000 trace calls, 100,000 trajectory events |
| Offline context-utilization evidence | 100,000 total ranges and hash inputs, 10,000 ranges per repository-generation/path, 1,000 context calls/ranges, 256 relevance paths, 4 KiB per path |
| Experimental Git-history lane | 256 pinned ancestors, 2 Git subprocesses, 4,096 output lines, 32 KiB per line, 4 current paths |
| Experimental AST structural lane v1 | 16 KiB failure-trace input, 2 languages, 8 AST-derived terms, 16 structural definitions / 1,024 tokens per term, 2 soft focus paths |
| Experimental AST structural lane v2 | 16 KiB failure-trace input, 2 languages, 8 structural terms, 4 qualified-owner terms, 4 named-argument/object-field terms, 16 hits / 1,024 tokens per term, 2 diagnostic paths, 1 exact owner excerpt / 128 source tokens reserved inside the task budget and suppressed by content hash after the first turn |
| Experimental orientation capsule | 1 AST owner path, 4 matched terms, 4 definitions, 128 exact tokens |

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

Path-scoped required evidence uses the same paged constraint scan and
lexicographically retained four-file bound as focus-local generation. It checks
at most 256 indexed chunks per retained file for case-insensitive literal
queries, retains at most eight deterministic match locations per contract, and
materializes at most 40 lines centered on each retained location. Ranking
reserves enough candidates to cover the requested number of distinct queries
before must-path fallback and ordinary selection. Coverage keeps path scope and
evidence scope separate: a required file prefix cannot satisfy an evidence
contract unless the returned or already-held fragment carries a matching query.
Must-path generation inspects at most 256 chunks from the first eligible file
for each of the existing 256 bounded patterns. It chooses the highest
task-matching chunk deterministically; a pattern with no task match performs
one batched stored-excerpt lookup and emits a `required_path_fallback`
representation covering at most the first 40 lines.

The offline `context_utilization` classifier consumes the existing model A/B
`tool-trace.json` and `trajectory.json` contracts. It performs no repository or
SQLite scan and adds no production telemetry writes. It binds matching artifact
identities, requires strictly increasing call sequences, validates every
repository-relative range, and fails above the bounds in the table. The report
keeps relevance-path proxies, explicit later hash inputs, receipt follow-ups,
exact/overlap rereads, missing token attribution, and absence of an observable
downstream signal as separate fields. None is relabeled as model reasoning or a
causal utilization score.

The benchmark-only Git-history lane first freezes at most 256 ancestors, then
submits the complete commit set and one merged workflow-symbol regex to one
pickaxe process. It ranks at most four current files by matching-commit count
and recency before reusing the existing context focus-minimum path. The runner
sets `GIT_NO_LAZY_FETCH=1`: a blobless partial clone reports
`history_objects_unavailable_without_lazy_fetch` instead of downloading history
or treating missing objects as a complete empty result. The lane performs no
production scan and does not change default ranking.

The benchmark-only AST structural lane tolerantly parses bounded observed
failure traces with the existing tree-sitter parser. It retains call references
from at most two declared task languages, normalizes at most eight terms, and
uses one bounded definition-only search per term. Paths rank by distinct
definition terms, hit count, normalized score, and lexical path; at most two
become soft context focus paths. Gold labels never enter discovery. The lane
adds no parser pass during indexing, performs no file scan, does not force a
fragment quota, and does not change default production ranking.

The optional benchmark orientation capsule reuses the AST lane's already-ranked
definition hits, so it issues no additional search or parser work. It serializes
at most one owner path, four matched trace terms, and four indexed definition
names, dropping definitions and then terms until the complete routing artifact
fits 128 exact tokens. The capsule is reported separately from context
fragments and token budgets: it is a follow-up route, not selected source,
downstream-use evidence, or a production response contract.

The 12-query context planner retains up to four early domain terms and two
high-specificity terms selected from the remainder of the complete task.
Natural-language tasks retain at most two deterministic bigrams; tasks with
technical atoms retain one bigram while reserving up to four exact-atom slots.
This reduces sentence-order sensitivity without making query fan-out depend on
task length.

Opt-in workflow evidence shares that same 12-query ceiling and the existing
per-query symbol, reference, and FTS hit caps. The caller may supply at most
eight directly observed failure traces, symbols, repository-relative paths,
and test intents per class. Each item is capped at 8 KiB and all four classes
at 32 KiB combined. Evidence order is preserved; deterministic class quotas
reserve lanes before the ordinary task planner fills the remaining query
slots. Test intent contributes bounded path-prior scoring but does not trigger
an additional executable search lane. Empty evidence delegates to the original
planner unchanged. This contract adds no storage, filesystem scan, concurrency,
or memory fan-out beyond the validated request payload and existing query
bounds.

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

Occurrence grouping is a response projection over the already ranked and
paginated exhaustive page; it performs no additional repository or SQLite
scan. A bounded hash map shares identical path/range/content-hash excerpts, and
coordinates-only mode shares one group per selected path. Unique excerpts,
rather than repeated hits, consume the source-token budget. Coordinates-only
mode consumes zero source tokens, but its complete coordinate arrays still
count against `max_results`, `max_response_tokens`, and the final serialized
response accounting. The MCP initialize version hashes the deterministic
runtime tool catalog; computing that fingerprint adds no storage or retrieval
fan-out.

Every retrieval operation accepts an optional serialized service-response
ceiling through `ServiceCallOptions`, MCP `max_response_tokens`, or CLI
`--max-response-tokens`. This boundary counts the final compact service DTO,
including paths, diagnostics, receipts, metadata, and the accounting fields
themselves. It does not count MCP `CallToolResult` duplication, JSON-RPC
framing, or human CLI rendering. `token_budget`/`--budget` remains the
independent source-content ceiling.

Context response profiles are presentation projections owned by `Services` and
passed through `ServiceCallOptions`; they are not retrieval modes. `compact`,
`balanced`, and `explain` share candidate generation, ranking, selected
fragment identity and order, source budgets, hard constraints, and receipt
suppression. Compact removes optional individual omission, facet, and diff
detail while retaining fail-loud coverage, warnings, routing, aggregate
omission counts, and receipt evidence. The selected profile starts no scan,
query, storage write, or concurrency fan-out. Legacy
`verbose_diagnostics=true` normalizes to `explain`.

Fitting is deterministic and happens inside `Services`, after candidate
generation but before receipt evidence is committed. It first removes bounded
omission facets, detailed diff evidence, routing detail, and ranking reasons.
Only requests without include, must-cover, focus, diff, strict-scope, or
handoff constraints may then drop lowest-ranked selected fragments. Constrained
requests return a typed `RequestLimitExceeded` error when their correctness
skeleton cannot fit; fitting never weakens their coverage contract. Balanced
plan-only diff context omits detailed diff evidence; compact always omits
optional diff evidence and explain includes it when available. Receipt sizing
reserves the exact request/generated receipt identifier plus conservative
counter and warning shapes, and the final postcondition is checked after
receipt application.
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

Successful-result projection is fixed for explicit `dual`, `text`, and
`structured` modes. Explicit `auto` starts in fail-safe `dual` and may change
only during the MCP initialize handler. Its immutable registry requires exact
client name/version, negotiated protocol version, and current deterministic
tool-catalog digest; resolution is shared by handler clones and the bounded
transport through one atomic wire-mode value. Missing or malformed initialize
never reaches a smaller mode, and any key mismatch remains dual. Registry
lookup is a fixed in-memory row scan with no filesystem access, subprocess,
network call, storage write, or concurrency fan-out. Adding a row requires a
code-reviewed real-host evidence receipt and a catalog-digest test.

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
than current content. With `delta=true` and no `expected_hash`, the registry
selects the newest compatible base for the same repository and exact target by
reverse-scanning at most its existing 128 insertion-order keys. This adds no
unbounded index, storage, fan-out, or concurrency. An unchanged target returns
`not_modified`; any coordinate change fails safe to full current content.
Ordinary reads never consult or populate the registry. Search and outline never
invent empty successful results at generation zero.

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
  multi-repository coordination mechanism. Each process establishes at most
  eight read connections, so the aggregate bound grows linearly with the
  number of processes sharing a cache.
- Response token accounting is best-effort telemetry. Its zero-timeout SQLite
  write can be skipped under cross-process writer contention; retrieval
  correctness and generation publication never wait for that observation.
- The Linux release-mode multi-process profile runs 1, 4, and 8 stdio MCP
  processes in shared-cache and independent-cache A/B/B/A order. It separates
  cold startup, files/search/read/context warm rounds, idle CPU, and a forced
  periodic-poll phase; records aggregate CPU, CPU per operation, wall p50/p95,
  RSS, threads, connections, watcher admission, generation publication, and
  takeover; and requires complete normalized response parity. Its host-local
  CPU thresholds identify evidence for host-wide admission work, not permission
  to weaken retrieval, snapshot, or publication invariants and not automatic
  authorization for a shared daemon.
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

Markdown and LaTeX use custom document parsers instead of tree-sitter. The
LaTeX parser makes one bounded pass over the already size-limited indexed
source, then performs linear section-range closure and bounded sorting of the
facts it found. Memory is proportional to recognized facts plus section and
environment nesting; it does not retain a syntax tree. It ignores comments and
verbatim-like environments, marks malformed brace/environment structure
incomplete while retaining recovered facts, and uses the same section, label,
bibliography, citation, reference, and input/include facts for outline, exact
read, reference search, and import resolution.

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

Strict focus expansion stays inside the existing per-pattern bounds: at most
four eligible files are inspected, with at most 256 chunks and 128 symbols per
file, and at most eight distinct ranges retained per focus pattern. An exact
`focus_symbol` found in that bounded file-local symbol set is candidate evidence
even when the task text has no lexical overlap. If those bounded candidates
cannot satisfy `minimum_fragments_per_focus_path`, the response reports the
generated and requested counts instead of treating path presence as coverage.

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

The model A/B harness can attach an optional benchmark-only orientation capsule
to a prewalk handoff. A capsule contains exactly one safe relative owner path,
one to four nonempty query terms, at most four nonempty definition names, and at
most 128 exact source tokens across its serialized entries. The adapter rejects
missing, extra, or rewritten capsules and separately counts the complete
injected instruction and JSON wrapper. These bounds apply only to experimental
artifacts: the capsule does not change production candidate generation,
ranking, context selection, or any public protocol.

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

Evaluation-only frozen-holdout tooling is outside the application hot path. It
reads at most 32 MiB per policy, task, label, host, or receipt artifact and at
most 1,000 JSONL records. It performs no network requests, repository scans, or
task execution while sealing. Public receipts contain artifact commitments and
aggregate strata only; private labels must use safe relative paths, and Unix
sealing rejects label files readable by group or other users. Output uses
create-new semantics so a previous seal cannot be overwritten.

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
