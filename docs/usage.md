# Usage and tool reference

LeanToken exposes the same retrieval services through its CLI and MCP server.
All paths are relative to the configured repository root, and all source
responses are bounded.

## Global options

```text
--root <PATH>      Repository root (default: current directory)
--allow-broad-root Allow a filesystem root, home directory, or parent of home
--include-generated Include known generated and package-cache directories
--index-include <PATTERN> Include only matching repository-relative paths (repeatable)
--index-exclude <PATTERN> Exclude matching repository-relative paths (repeatable)
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
leantoken coverage
leantoken savings
leantoken doctor [--client CLIENT] [--ready-timeout-seconds SECONDS]
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
leantoken setup [CLIENT...] [--all] [--refresh] [--private-runtime] [--yes]
                [--dry-run] [--allow-outdated] [--force-unmanaged]
leantoken remove [CLIENT...] [--all] [--yes] [--dry-run] [--force-unmanaged]
leantoken runtime list
leantoken runtime prune [--keep-latest COUNT] [--dry-run] [--yes]
leantoken cache list [--summary] [--state STATE] [--repository-root PATH]
                     [--compatibility CLASS] [--index-content-version VERSION]
                     [--incompatible-with-current] [--limit COUNT] [--cursor CURSOR]
leantoken cache prune [--older-than DAYS] [--max-total-bytes BYTES]
                      [--remove-missing-roots] [--incompatible-with-current]
                      [--dry-run] [--yes]
leantoken episode audit --adapter ADAPTER --input PATH
```

Use `leantoken <command> --help` for the complete argument list.

### Episode auditor

`episode audit` normalizes one existing, already-redacted analyzer report into
a stable local report. It is repository-free: repository, database, indexing,
and tokenizer options are rejected. The default output is deterministic
Markdown; pass global `--json` for deterministic compact JSON:

```bash
leantoken episode audit \
  --adapter multi-agent-suite-v1 \
  --input benchmarks/reports/multi-agent-context-suite-v1-codex-0.144.1.json

leantoken --json episode audit \
  --adapter mcp-wire-report-v2 \
  --input benchmarks/reports/wire-trace-synthetic-v2.json
```

Adapters are explicit and versioned:

- `multi-agent-suite-v1` imports `codex_multi_agent_suite` aggregates;
- `model-ab-trajectory-v1` imports `model_ab_trajectory` classifications;
- `mcp-wire-report-v2` imports current `mcp_wire_analyze` reports;
- `codex-host-receipt-v1` imports publishable `codex_host_receipt` reports;
- `context-utilization-v1` imports `context_utilization` classifications.

An adapter/schema mismatch fails instead of guessing. The suite adapter
recomputes counts, provider-request means, provider-input comparisons, and
contract violations from complete redacted run samples, then checks the
published aggregates. It therefore reproduces the 60-run v1 mean of 8.2 child
provider requests and 50.9% input regression, and identifies the v2
one-context-plus-optional-search contract without reading private rollouts.
Wire and host adapters share the same normalized output while keeping absent
provider accounting `null`.

The auditor reads at most 64 MiB and accepts at most 10,000 episodes, 100,000
tool calls, 100,000 events, 100,000 evidence ranges, and 4,096 distinct
artifact bindings. JSON input is parsed once and output cardinality is bounded
by those limits. Reports contain only normalized counts, fixed classifier
descriptions, and BLAKE3/Git bindings. Input paths, task names, prompts, raw
source, commands, tool arguments, and tool outputs are not copied. A host
receipt whose privacy declaration retains private material is rejected.

Coverage accompanies every nullable count. `complete` means complete within
the imported analyzer's declared boundary, `reported_subset` names a
conservative child/tool subset, and `unavailable` serializes a `null` value.
Local tokenizer counts never populate provider-native input. Downstream-use
signals remain explicit proxies; absence of a signal is not labeled unused
evidence. The report separately lists all eight v1 avoidable-event classifiers
as `exact`, `proxy`, or `unavailable`, so an unevaluable classifier cannot be
mistaken for an observed zero.

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

`leantoken coverage` explicitly scans indexed file metadata from one pinned
generation and reports structural parser coverage. It separates recognized
complete, recognized incomplete, and unrecognized files, includes exact source
byte totals, and returns bounded language and safe extension-family groups.
Pass `--json` for compact machine-readable output. The report does not walk the
working tree, reparse source, or expose repository paths.

### Indexing scope

By default, LeanToken indexes the complete repository visible through Git,
`.ignore`, and `.leantokenignore` rules, except for `.git` metadata and the
conservative generated-directory policy. An explicit indexing scope reduces
the repository membership built into SQLite:

```bash
leantoken --root . \
  --index-include 'src/**' \
  --index-include 'tests/**' \
  --index-exclude 'src/generated/**' \
  index
```

Includes are optional; with only excludes, every other ignore-visible path is
eligible. Excludes always win. Literal paths select their complete subtree,
while patterns use the same slash-normalized, case-sensitive glob behavior as
retrieval path filters. Patterns must be repository-relative. LeanToken
normalizes separators and redundant `.` components, sorts and deduplicates the
result, and accepts at most 64 include-plus-exclude patterns, 1,024 bytes per
pattern, and 16 KiB in total.

Scope is an indexing and negative-evidence boundary, not a query-time
convenience filter. Discovery prunes excluded subtrees before traversal limits
and preparation; status reports the normalized patterns and a compact opaque
scope digest. Every retrieval response reports `meta.index_scope` as `full` or
`scoped` and includes the digest for scoped caches. An empty result from a
scoped cache therefore proves absence only inside that configured scope.

Normalized scope participates in the automatically managed cache identity, so
full and scoped indexes for one repository can coexist. Reuse the same scope
arguments on every command that must address that cache. An explicit
`--database` is bound to both repository and full scope identity and fails
with `index_scope_mismatch` if reused with another scope.

For a dependency-heavy TileLang checkout, for example, first-party work can
exclude the recorded dependency submodules:

```bash
leantoken --root . --index-exclude '3rdparty/**' index
leantoken --root . --index-exclude '3rdparty/**' mcp
```

Workspace-specific MCP registrations can place the repeatable scope flags
before the `mcp` subcommand in their command arguments. The global `setup`
flow intentionally remains repository-agnostic and does not infer a scope.
Changing scope selects another managed cache; it never mutates the membership
meaning of an existing cache silently.

After the first generation, the one-shot `files`, `search`, `outline`, `read`,
and `context` commands default to `--consistency reconcile_working_tree`. Each
command completes a non-rebuild reconciliation before opening one committed
snapshot, so edits completed before the command are visible atomically. Use
`--consistency indexed_generation` when a lower-latency query of the latest
completed snapshot is intentional. Changes written concurrently may require a
later request.

`leantoken savings` reports persistent repository-local observed token
accounting for complete serialized responses. Search, outline, and context
compare emitted source with whole-file reads of the unique represented files.
Read compares the emitted range with the requested live range before
truncation or suppression.

The nested `response_accounting` object is the machine-readable retrieval-
compression section and additionally covers `files`,
`context_plan`, `json`, and `history`. It separates source,
path/metadata, protocol, and total compact-response tokens. JSON uses the
complete input file or files as its represented-source baseline. Operations
without a defensible source baseline still contribute their full response cost,
so their signed `estimated_net_tokens_saved` value is negative rather than
silently disappearing. Counts are stored separately per configured tokenizer.

The default terminal view separates **Retrieval compression** from **Observed
task savings** before the per-operation table. The represented-source ratio is
labeled retrieval-only and is never presented as a task-savings percentage.
Color is used when stdout is a terminal. `NO_COLOR` or `CLICOLOR=0` disables
color, while `CLICOLOR_FORCE=1` enables it for compatible redirected output.

Pass `--json` for the compact JSON representation used by scripts. The existing
`response_accounting` object owns successful-response retrieval compression;
the additive `observed_task_savings` object reports task attribution
separately. Without a host task/outcome identity its status is `unavailable`,
its task delta, rate,
retry, superseded, relevance, and failure-response-token fields remain `null`,
and the successful responses with unknown relevance and observed failed calls
remain visible as counts. The `observations` object reports persisted
successful and failed service records, exact `expected_hash` not-modified
responses, their suppressed represented-source tokens, and a fixed-order
failure breakdown with non-sensitive categories. Its legacy `useful`
classification means a complete supported protocol response, not relevant or
useful evidence; the human view labels it `complete-supported`.

Full-response counts include the compact structured response but not tool
discovery, JSON-RPC transport envelopes, provider billing/cache behavior,
native-tool costs, or task/evidence success. Instrumented service failures are
counted separately and never assigned a fabricated token cost. Calls cannot be
grouped into retry chains or tasks because LeanToken receives no host
task/outcome identifier. It also cannot know whether evidence was unused,
irrelevant, or superseded, nor whether a task completed successfully; these
limits are returned explicitly in `observations.unobserved`.

Accounting is best effort: a busy repository writer skips telemetry rather
than delaying or failing retrieval, so persisted counts are lower bounds.
Whole-file baselines are also unavailable when the selected tokenizer does not
match the indexed tokenizer until reconciliation completes. Source-only
counters from older caches remain visible, but cannot be reconstructed as
historical full-response costs and are therefore excluded from
`response_accounting`. This is not an audit ledger.

Current index responses retain the aggregate `files_skipped` count and explain
it with the bounded `skip_reasons` object: `binary`,
`oversized_during_read`, and `failed`. These counts cover files admitted for
preparation and always sum to `files_skipped`. Older serialized responses can
omit the object because their breakdown is unknown. No per-file skip list is
returned; bounded failure warnings may still identify files that could not be
read. `files_seen` counts admitted files plus deletions directly observed from
requested targeted paths. Paths merely omitted by full or visibility discovery
because they are absent or excluded are not part of `files_seen`,
`files_skipped`, or the reason counts. An already-indexed omitted path can still
increment `files_removed` when its stale entry is deleted.

## MCP setup and version lifecycle

Setup writes only the `leantoken` entry in each selected global client config.
It manages a concise discovery skill only for the selected host family: Claude
Code uses `~/.claude/skills/leantoken/SKILL.md`; Cursor, OpenCode, Codex,
Gemini CLI, and Antigravity use `~/.agents/skills/leantoken/SKILL.md`. Hosts
preload only its name and routing description, then load the instructions on
selection; the nine MCP schemas remain deferred. Repeated setup updates only
marker-owned skill copies, removal preserves an unowned file, and partial
client removal retains a shared skill while another managed registration needs
it. JSON setup reports the exact `cl100k_base` size of one discovery skill as
telemetry; it is not a pass/fail cap on the routing guidance.

New MCP launchers carry a hidden setup ownership marker. Exact-version npx
launchers created by earlier releases and executables below LeanToken's private
runtime root are recognized for migration. A same-name manual registration is
otherwise left untouched and setup fails with its exact path. Review that file,
then pass `--force-unmanaged --dry-run` to preview the replacement before
applying it explicitly.
When setup runs through npx, the stored command pins
`leantoken@<exact current version>` and retains `--yes` so background MCP
startup cannot block on an install prompt. The launcher may contact npm to
resolve or download that exact package, but it cannot switch to a newer version
between restarts.

For regular use, the private native runtime is recommended to avoid retaining
npm and Node wrapper processes for every MCP session. It remains explicit so a
zero-install setup does not silently add an application-data write:

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
versioned runtimes for explicit rollback; it never selects `latest`. Private
installation rejects a runtime root that is a symlink or is not a directory.
JSON reports a transaction-wide `apply_error` even when an orphan discovery
skill was the only planned mutation and no client result rows exist.

`runtime list` reports installed versions, executable bytes, active state, and
client references. `runtime prune` is a dry-run unless `--yes` is present; it
always retains referenced and active runtimes, keeps the newest two
unreferenced versions by default, and refuses directories containing anything
other than the expected native executable. Applied pruning rechecks every
supported client configuration immediately before deletion and removes through
snapshot-matched open directory handles, so a concurrent registration or path
swap fails closed. Change the bounded retention window with `--keep-latest` (0
through 64):

```bash
leantoken runtime list
leantoken runtime prune --keep-latest 2 --dry-run
leantoken runtime prune --keep-latest 2 --yes
```

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
OR filters, and `--repository-root` matches one exact recorded root.
`--compatibility` independently filters `compatible-current`,
`obsolete-older`, `legacy-unversioned`, `newer-unsupported`, or `unknown`;
`--index-content-version` is an exact repeatable filter, while
`--incompatible-with-current` keeps only older and legacy-unversioned content.
At most five compatibility and 32 exact-version filters are accepted. All
filters are bound into the versioned cursor and cannot be changed between
pages.

Entry pages retain the existing metadata `state` and separately expose content
`compatibility`, full/scoped index identity, recorded root, schema, last
access, direct SQLite/sidecar bytes, and active lease status. Summaries report
entries/bytes by compatibility plus inactive incompatible bytes that are
actually safe to reclaim. Legacy repository-only identities remain visible.
Listing does not open repository services and therefore works from any
directory. JSON output contains Unix timestamps and
returned/matched/total counts for automation.

The versioned identity applies to automatically managed caches. An explicit
`--database` path remains unchanged and must not be shared by incompatible
index-content versions or normalized index scopes.

`cache prune` requires at least one explicit selection policy:

- `--older-than DAYS` selects caches whose last repository bind is at least that
  old;
- `--max-total-bytes BYTES` selects least-recently-used caches until the managed
  total reaches the requested bound;
- `--remove-missing-roots` explicitly selects a cache when its recorded root is
  currently absent;
- `--incompatible-with-current` targets recognizable `obsolete-older` and
  `legacy-unversioned` caches; active entries are reported but skipped. Without
  `--yes`, this criterion automatically performs a dry run.

Use `--dry-run` to inspect every keep/delete/skip decision. Actual deletion
requires `--yes`. Missing roots are not an implicit deletion criterion because
offline mounts and removable volumes can return later. Older-schema and
recognizable incomplete caches remain eligible for explicit age or size
policies. Corrupt/unknown inspection results, newer content, newer schema,
mismatched root identity, and unexpected directory content always fail closed.
After acquiring the exclusive lease, prune re-inspects compatibility and safety
before deleting to prevent a stale plan from crossing a state change.

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
verifies its initialization identity and agent instructions, exact nine-tool
catalog, and first `leantoken.context` retrieval. On a cold repository it
allows up to 120 seconds by default for the first retrieval, then follows
structured `retry_after_ms` guidance while the index warms. Set
`--ready-timeout-seconds` from 1 through 600 for a different bounded window.
If the window expires during normal indexing, the diagnostic says that the
index is still building and tells you to rerun after it completes. Use `--json`
for a machine-readable readiness report, including the current executable,
configured host registrations and their inferred versions, and the executable's
`index_content_version`. This doctor launches the current executable and
compares it with configured host entries; it does not claim to identify other
unregistered processes that share an explicit database.
Pass `--client codex` (or another supported client) to read that host's stored
registration and launch its exact command and arguments instead. This verifies
the configuration users actually restart into, including pinned npx and private
runtime launchers. When the registration exposes an exact pinned release, the
handshake and tool catalog are validated against that configured release rather
than the version of the doctor process that launched it. Aggregate doctor output
reports an exact but disabled OpenCode entry as `disabled` and recommends a
managed refresh instead of calling it current. The configured-child MCP contract
does not expose its index schema, so this mode omits `index_content_version`
instead of reporting the launching doctor's compile-time value.
Failures use the `doctor_failure` category and identify the `registration`,
`launch`, `handshake`, `catalog`, or `first_retrieval` stage so repair tooling
does not need to parse prose.

## MCP server

### Breaking request contract

The MCP request contract is generated from the same Rust types used by the
RMCP `Parameters<T>` adapters. Clients should use the published schemas rather
than reconstructing a second schema or protocol-version handshake. `files`,
`search`, `json`, and `history` requests select one tagged operation under
`operation`; controls that apply only to that operation are nested in the
selected variant. For example, search uses
`{"operation":{"kind":"regex","query":"unsafe\\s+fn"}}` and file discovery
uses `{"operation":{"kind":"glob","pattern":"src/**/*.rs"}}`.

The breaking shape change also makes symbol identity explicit. Outline and
history results use `{ "name": "method", "parent": "Type" }` where a parent
exists, and read/history inputs pass the same object rather than asking the
client to rebuild a `parent.name` string. Repository-relative paths and
non-empty selectors are normalized at the request boundary. Optional numeric
limits treat omission and explicit `null` identically and use the documented
default. Relationships that JSON Schema cannot express, such as
`minimum_query_matches <= queries.length`, remain runtime validation errors.

Ordinary context responses may include bounded `provenance` with commit,
branch, working-tree state, repository generation, freshness, and an explicit
availability status. Provenance is best effort: an unavailable Git probe does
not invalidate otherwise valid context. The `+contract.<fingerprint>` suffix
is only LeanToken's application-capability diagnostic digest over the generated
tool catalog and LeanToken resource metadata; RMCP owns MCP protocol
negotiation.

`leantoken mcp` starts the stdio protocol before opening the repository cache so
the initialize handshake is never blocked by indexing. After the client's
initialized notification, one process becomes indexing leader and followers
reuse its committed SQLite generations. A retrieval call made while the first
generation is being built waits internally for up to 30 seconds so a short cold
index does not require another model turn. This same absolute bound applies when
an explicit `reconcile_working_tree` call is waiting behind the initial
generation's operation lock. If the requested consistency boundary cannot
finish within the bound, its waiter is removed and the call returns successful
structured `index_building` retry guidance with `retry_after_ms`; it never falls
back to a stale result. Later calls report whether they use a current or
reconciling index generation.

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
`.rustup`, and always prunes `.git` metadata before descending. Use
`--include-generated` only when generated trees are intentional source inputs;
it never admits `.git`.

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

The default `structured` mode returns the typed result without duplicating its
JSON as text. Explicit `dual` and `text` modes remain troubleshooting
overrides. `leantoken doctor` reports the effective static mode directly.

The catalog publishes documented input schemas but omits
optional output schemas; repeating full response DTOs in every `tools/list`
result costs model context without changing tool behavior.

Search, outline, read, and context responses return an opaque
`meta.receipt_id`. Pass that ID to a later retrieval to suppress exact content
and overlapping source ranges already returned. Receipt metadata persists in
the repository cache, so the same ID works across MCP processes and
programmatic service restarts while that cache, generation, and the sliding
24-hour lifetime remain valid. Near-duplicate evidence remains visible and increments
`meta.receipt_near_duplicates`.

Receipts are bounded, server-managed, and tied to one repository generation.
An old-generation receipt returns `stale_receipt`; malformed, expired,
capacity-evicted, cross-cache, and unknown IDs return `unknown_receipt`.
Deleting/pruning the repository cache also deletes its receipts. Receipts store
only repository identity, generation, paths, line ranges, content hashes, and
semantic signatures—not task/query text or raw source. Context
`fragment_hashes` and `known_hashes` remain available for stateless
compatibility.

Use `leantoken.receipt_rebase` only when carrying exact evidence across a
completed generation is useful. It is explicit and never changes ordinary
stale-receipt behavior:

```json
{
  "receipt_id": "r...",
  "consistency": "reconcile_working_tree",
  "max_samples_per_outcome": 4,
  "max_response_tokens": 2000
}
```

The source receipt must belong to the same repository cache and indexing scope
and to an earlier generation. Evidence is carried only when current path,
inclusive line coordinates, and content hash are all identical. The operation
does not guess line shifts, renames, moved symbols, duplicate bodies, overlap,
near-duplicates, or fuzzy matches. It returns complete `carried`, `changed`,
`missing`, and `unmapped` counts, a BLAKE3 commitment to the ordered full
classification, and up to 16 source-free examples per outcome. The new receipt
is `meta.receipt_id`; the source remains unchanged and stale.

Carried rows are exact-only: they suppress only a later candidate with the same
emitted-content hash. They do not participate in range-overlap or
near-duplicate suppression, so an unchanged outline signature cannot hide a
changed body in the same range.

Validation is bounded to the source receipt's 2,048 evidence rows, 64 MiB of
live source in total, one configured-size file buffer at a time, and 64
exact-coordinate structural candidates per evidence item. Anything that cannot
be proven inside those bounds is `unmapped` and is not carried. Generation or
source-receipt races fail without creating a partial receipt. This Phase 1
contract intentionally excludes automatic rebase and all relocation heuristics.

Prefer LeanToken over shell discovery and whole-file reads. For a broad coding,
debugging, review, or architecture task, start with `leantoken.context`. Use the
narrow tools directly when the target is already known:

```text
broad task -> context
known identifier/text -> search -> read
known file, unknown range -> outline -> read
unknown path -> files
```

Index-backed MCP retrieval tools, including `receipt_rebase`, accept an
optional `consistency` input:

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
MCP, and direct service APIs. `max_results` must be in `1..=100`; the active
repository cap is `config.max_results` and may be lower. Omitted values use
`config.default_results`.
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

## `leantoken.receipt_rebase`

This explicit evidence-lifecycle operation is described in the
[MCP receipt section](#mcp-server). It returns no source and performs no
retrieval ranking. Call it only after a stale receipt is worth preserving; use
the returned current-generation `meta.receipt_id` on later `search`, `outline`,
`read`, or `context` requests.

## `leantoken.savings`

Returns repository-local token accounting and an opaque `snapshot`. With no
input, `window` is `lifetime`. Supply a prior
`{"snapshot":"lts1..."}` to receive only aggregate counter changes since that
snapshot; the response returns a replacement snapshot. Snapshots are
caller-carried, repository- and tokenizer-bound, checksummed, and limited to
32 KiB. They do not create a per-request event table.

`response_accounting` retains every successful response and reports comparable baseline counts,
source/path-metadata/protocol/total response tokens, signed response deltas,
receipt-suppression counts, and fixed rows for all nine retrieval operations.

The additive `observations` object reports best-effort persisted successes,
failures by operation and stable error category, and exact `expected_hash`
matches. `expected_hash_suppressed_source_tokens` measures the requested source
omitted by those matches. This is distinct from
`response_accounting.receipt_suppressed_exact` and
`receipt_suppressed_overlap`, which describe receipt-based duplicate
suppression. `request_classification` divides new requests into `useful`,
`incomplete`, `unsupported`, `hash_suppressed`, and `failed`.
Typed `unsupported_language` failures belong to `unsupported`; the broader
`failed_service_requests` observation still reports every returned error.
Incomplete or unsupported retrievals—including a zero-symbol LaTeX
outline—remain visible as full response cost but do not inflate effective
source compression.

The report is a represented-source comparison over successful recorded
responses plus separately observed failure counts. It does not infer retry
chains, whether returned evidence was used, superseded calls, provider framing,
or task success. Those limits are returned in `observations.unobserved`. Treat
the signed response delta as local response accounting, not as a
correctness-adjusted claim about an agent's full workflow.

This is a read-only observation: calling `leantoken.savings` does not update
the tracker. Accounting writes never delay retrieval: local writer contention
skips the record, so every count is a persisted lower bound.

## `leantoken.files`

Discovers repository structure without returning source bodies.

Operations:

- `{"kind":"tree","path":"src","depth":2}`: compact hierarchy;
- `{"kind":"find","query":"mcp"}`: fuzzy path and basename matching;
- `{"kind":"glob","pattern":"src/**/*.rs"}`: indexed path matching.

Pass one of those tagged objects as `operation`. Operation-specific fields
cannot be mixed. `max_results`, `max_response_tokens`, `cursor`, and (for
`tree`) `depth` belong to the selected operation. Output contains bounded
file/directory entries with language and size metadata when available.

Set `projection="paths"` for the opt-in path-only response. It returns the same
ordered page as `full` in a `paths` array plus the complete `meta` freshness,
repository, token-accounting, and continuation contract. Kind, language, byte
size, and fuzzy score are omitted. The default remains `full`; use it when those
fields affect the next routing decision.

## `leantoken.search`

Returns ranked source excerpts. Modes are `auto`, `text`, `regex`,
`identifier`, `symbol`, and `reference`.

Each request selects a tagged operation such as
`{"kind":"regex","query":"unsafe\\s+fn","max_results":20}`. Its options
include path filters, focus paths, result and token limits, context-line count,
case sensitivity, and a generation-bound cursor. Defaults are 20 results,
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

Structural symbol queries accept either a bare `name` or the canonical
`parent.name` identity exposed by outline and search metadata. Qualified
queries use the leaf name to obtain trigram candidates and then verify the
combined owner/name identity, so they do not require a larger FTS index.
Unqualified search remains a bounded candidate operation and may return more
than one definition; combine each hit's `enclosing_symbol` and `symbol` to pass
the qualified identity to outline, read, or history.

Set `projection="grouped"` for broad symbol/reference discovery that does not
need every repeated excerpt or score. Grouping runs after the normal ranked
page, exact-hit deduplication, lexical/structural definition merge, and receipt
filtering. Each group retains an explicit definition when available, otherwise
one representative path/range/excerpt/content hash; references are summarized
by file with their count, covered line span, and roles. `coverage`,
`occurrences_total`, repository freshness, exact token accounting, and the
normal continuation cursor remain available. Non-exhaustive searches still
default to `full`. Use `leantoken.read` with a returned path/range to expand
source, or repeat a narrowed `full` search when individual reference excerpts
are needed.

Set `all_occurrences` in `text` or `regex` mode for every non-overlapping match,
including repeated alternatives on one line. MCP defaults these requests to
`projection="occurrences"`: one path/range/excerpt/content hash per unique
excerpt plus an array of every exact `{line, start_column, end_column}` span.
Columns are zero-based UTF-8 byte columns; a multi-line regular expression also
reports `end_line`. Set `coordinates_only=true` to group only by path and omit
source and hashes, or explicitly request `projection="full"` for the older
per-occurrence ranked hits and global byte offsets.

Both shapes report `occurrences_returned` for the current page and an exact
`occurrences_total` across the filtered index. Grouped excerpt token charging
counts each unique excerpt once; coordinates-only calls charge no source
tokens. Exhaustive pagination still applies `max_results` without changing the
total; follow `next_cursor` until absent.

For a repeated exact exhaustive query, MCP callers can explicitly request a
persistent coverage receipt:

```json
{
  "operation": {
    "kind": "regex",
    "query": "unsafe\\s+fn",
    "all_occurrences": true,
    "coordinates_only": true,
    "query_receipt": {"kind": "record"}
  }
}
```

`record` persists a receipt only when every occurrence fits the returned page
and the final response budget succeeds. Pagination or token omission returns
`status: "not_recorded_incomplete_response"` without an ID. Invalid regex,
internal exhaustive limits, cancellation before the receipt write, and any
other error persist nothing. Ranked `auto`, `identifier`, `symbol`, and
`reference` modes, focus boosts, cursors, the `full`/`grouped` projections, and
evidence `receipt_id` cannot be combined with query receipts.

Pass the returned ID back with the same normalized predicate to avoid repeating
the lexical scan:

```json
{
  "operation": {
    "kind": "regex",
    "query": "unsafe\\s+fn",
    "all_occurrences": true,
    "coordinates_only": true,
    "query_receipt": {
      "kind": "reuse",
      "receipt_id": "q..."
    }
  }
}
```

A successful reuse returns `status: "already_covered"`, the exact match count
and result commitment, and no occurrence groups. Path separators, duplicate
patterns, and pattern order are normalized. An exact zero-match proof may also
cover a conservatively provable narrower include/exclude scope; a nonzero
superset cannot derive a subset count and fails with
`query_receipt_mismatch`. A later repository generation is reusable only when
the index configuration and a streaming digest of every relevant indexed
path/content hash are unchanged. Otherwise the call fails with
`stale_query_receipt`.

This reuse still requires a tool call, so it can avoid the server-side
text/regex scan and result payload but cannot claim that the host skipped a
model turn. A future handoff or capsule integration would need separate host
evidence before claiming an avoided call.

The MCP initialize response appends `+contract.<fingerprint>` to the runtime
version. The fingerprint covers the canonical generated tool catalog and
LeanToken resource capability/template metadata, so clients can distinguish a
stale server binary or cached application contract from the feature set that
accepted the request. RMCP negotiates the MCP protocol version separately.

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

The name filter accepts the same bare or canonical `parent.name` identity used
by symbol search, read, and history. A bare name can intentionally return
multiple definitions; use the returned `parent`, `kind`, and line coordinates
to choose a qualified target.

`path_results` returns one ordered outcome for every input with its zero-based
`request_index`, normalized repository-relative `path`, and typed `status`.
Indexed paths continue to produce bounded entries in `files` when another path
is absent. Paths that are missing, ignored, or otherwise outside the current
index snapshot report `not_indexed`; LeanToken does not probe the live
filesystem to guess which cause applies. Invalid or unsafe paths remain
request-level errors.

`parse_complete` reports whether every requested path was indexed and parsed
completely; each file reports the same state independently. Parse completeness
does not imply result completeness.

`result_complete` is true only when every path was indexed and the response
contains every filtered symbol and import. Exact `total_symbols`,
`returned_symbols`, `total_imports`, `returned_imports`, and
`symbol_counts_by_kind` cover the indexed subset and make its coverage auditable.
`truncated_by_max_results` provides `meta.next_cursor` for another page, while
`truncated_by_max_tokens` means the query must be repeated with a larger token
budget to recover omitted entries. Outline cursors are bound to the repository
generation, normalized path order, symbol filters, and projection. Each page
repeats the same ordered `path_results`; the cursor's global entry offset maps
deterministically through the indexed subset in that same request snapshot.

Set `projection="signatures"` to exclude imports before result/token selection
and omit symbol byte offsets. The response retains path, language,
parse-completeness state, symbol name/kind/parent/signature, one-based line
ranges, exact symbol coverage, freshness, and continuation. Each file includes
a `content_hash` over the serialized ordered `signatures` array so the compact
representation can be checked and reused without paying one hash per symbol.
`result_complete` then describes the filtered symbol set, not imports. Signature
cursors are projection-bound; switching between `full` and `signatures` with a
cursor fails stale instead of applying incompatible offsets.

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
- `target: {"kind":"symbol","identity":{"name":"LeanTokenMcp"}}`
  selects one indexed symbol definition. Nested definitions use
  `{"name":"wait_for_initial_index_cancellable","parent":"Services"}`.
  An unqualified or qualified identity that matches multiple definitions
  returns typed `symbol_ambiguous` instead of selecting the first definition.
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
  base. When `expected_hash` is absent, a follow-up automatically selects the
  newest base for the same repository and exact target. Unchanged content
  returns `status: "not_modified"`; changed content uses `status: "delta"` and
  the `delta` field only when the complete unified diff costs fewer source
  tokens than full current content. Pass `expected_hash` to require one
  explicit prior hash instead.

`content_hash` identifies the returned range. `indexed_hash` identifies the
whole indexed file. `index_stale` is true when the live file differs from the
indexed version (for example after an edit that has not been reindexed yet).
`target_start_line` and `target_end_line` describe the complete resolved target;
`returned_start_line` and `returned_end_line` describe the current page.
`status: "truncated"`, `truncated: true`, `next_start_line`, and
`continuation_cursor` fail loudly whenever source remains. Continuation cursors
are bound to the repository generation, path, and live full-file hash, so a
stale cursor cannot combine pages from different file versions.
`truncation_guidance` reports the complete target and remaining source-token
cost, estimated additional pages at the current budget, a bounded recommended
budget for the next continuation, and the minimum pages allowed by the server's
source-token ceiling. `basis: "verified_live"` means full-file verification
proved the pinned indexed target matches live source;
`basis: "indexed_generation_estimate"` keeps bounded reads cheap and makes the
snapshot-based uncertainty explicit.
`delta_receipt` reports the stable target key, selected base and head hashes and
generations, full and delta token counts, avoided tokens, and any explicit
fallback reason. Missing bases, changed target coordinates, truncated or
oversized content, and uneconomic diffs return full content. Delta state is
in-memory and repository-local, expires after 30 minutes, and is bounded to
128 entries, 512 KiB per entry, and 8 MiB of retained content. Latest-base
selection scans only those 128 insertion-order keys in reverse and never
creates an additional unbounded index. It never applies
to ranked context fragments or continuation cursors.

`status: "not_modified"` means either `expected_hash` or the automatically
selected delta base matched. The distinct `status: "receipt_suppressed"` means
a server-managed evidence receipt already contained the exact current content.
Changed content in an overlapping range is returned and added to the receipt
rather than being hidden as unchanged.
`meta.repository_generation` is the committed index generation used for path
and symbol lookup; `meta.freshness` is `reconciling` while an index operation
is active on this cache.

When the index has never completed a generation, retrieval tools wait for up to
30 seconds before returning a successful retry result such as
`{"status":"retryable","reason":"index_building","retry_after_ms":500,
"index_progress":{"detail_available":true,"active":true,
"current_generation":0,"attempt_id":"...","phase":"preparation",...}}`.
Detailed progress has a fixed shape with aggregate counters only. Its phases
distinguish discovery, hash/planning, preparation, relational staging, each FTS
build, final commit/checkpoint, and terminal completion/failure/cancellation.
`update_sequence`, `last_progress_unix_ms`, and aggregate counters let callers
detect forward progress without exposing paths or source. `files_staged` is not
queryable until the atomic generation commit completes.

A follower process that cannot observe the leader's memory instead returns
`"detail_available":false`; unavailable optional fields are omitted rather
than reported as zero. The same `index_progress` object appears in read-only
status while the committed generation remains zero and is omitted once an
index is ready. Retry the same call after `retry_after_ms`. Caller cancellation
interrupts the internal wait. After local edits, set `consistency` to
`reconcile_working_tree` on the next MCP retrieval. An `indexed_generation`
read may still use `index_stale` and `expected_hash` to detect or suppress live
ranges.

## `leantoken.json`

Reads exact repository-relative JSON files without requiring them to be indexed,
including ignored artifact paths. Operations are:

- `query`: select the root, an RFC 6901 JSON Pointer, or a standard JMESPath
  expression, then return a `value`, `collapsed`, `keys`, or `schema`
  projection.
- `numeric_summary`: collect numeric leaves below the selection and return exact
  count, min, median, nearest-rank p95, max, and ignored non-numeric count.
- `diff_fields`: evaluate up to 100 selectors against two files and report
  presence, projected before/after values, and whether each field changed.

The JSON request has exactly these three operation kinds. `collapsed`, `keys`,
and `schema` are projections of `query`, not operation kinds. For example:

```json
{
  "operation": {
    "kind": "query",
    "path": "benchmarks/reports/graph-signal-ablation-v1.json",
    "projection": "keys"
  }
}
```

JMESPath selectors are evaluated against the selected JSON document root. To
summarize the graph benchmark's per-corpus cold-index values, select the actual
field beneath `graph_index.corpora`:

```json
{
  "operation": {
    "kind": "numeric_summary",
    "path": "benchmarks/reports/graph-signal-ablation-v1.json",
    "selector": {
      "kind": "jmespath",
      "expression": "graph_index.corpora[].cold_index_ms"
    }
  }
}
```

`consistency` belongs to applicable repository retrieval tools; it is not a
field in this live JSON request. A numeric summary with `count: 0` means the
selected path contained no numeric leaves, not necessarily that the JSON file
was malformed. `keys(@)` only works when its selection context is an object;
if a caller evaluates it against the wrong or null context, it fails. Use the
`query` operation with `projection: "keys"` for bounded key traversal.

`collapsed` replaces arrays with their total count and a bounded sample.
`max_items` defaults to 1,000 (maximum 10,000), `array_sample_size` defaults to
3 (maximum 20), and `max_tokens` defaults to 8,000. Exact source hashes bind
responses to the complete live files. Raw values that exceed a cap fail loud.

`keys` is a deterministic flat projection and supports pagination under both
item and token limits. MCP orders broad keys by `(depth, JSON pointer)` so root
and top-level shape precede deep subtrees. Optional `depth` is relative to the
selected root: root is zero, `depth: 1` includes immediate children, object
segments use RFC 6901 escaping, and arrays share one `/*` shape segment. Every
keys response reports exact `total_items`, `returned_items`, and
`remaining_items`. An incomplete page identifies `max_items` or `max_tokens` in
`incomplete_reason` and returns `meta.next_cursor`; repeat the identical path,
selector, projection, and depth with `cursor` to continue. Version-two cursors
bind the traversal order and depth in addition to those query inputs and the
live source hash. Legacy cursors, changed files, and changed query shapes fail
with `stale_cursor` instead of mixing incompatible pages.

Incomplete `schema`, `collapsed`, and projected diff results report the same
exact counts and `incomplete_reason`. Schema construction retains breadth-first
siblings until `max_items` or `max_tokens` is reached and adds an
`x-leantoken-incomplete` object containing the exact omitted-frontier count and
up to 32 deterministic JSON pointers. Their nested shapes are not split into
ambiguous pages, so they do not return a cursor; increase the relevant limit or
use a narrower selector. A complete schema retains the prior schema shape and
does not contain the extension. Strict JSON parse failures include the syntax
category, one-based line and column, and zero-based byte offset. JMESPath
compile and runtime failures include their stage, typed reason, expression
offset, line, and column.

## `leantoken.history`

Reads symbol-aware evidence from immutable Git revisions without changing the
working tree or index:

- `operation: {"kind":"read_symbol","path":"src/lib.rs","symbol":{"name":"Services"},
  "revision":"main~1"}` parses the historical blob and returns that symbol.
- `operation: {"kind":"diff_symbol",...}` parses the symbol independently at
  `base_revision` and `head_revision`, then returns a bounded unified diff.
- `operation: {"kind":"diff_symbols","targets":[...],...}` compares an ordered
  symbol set over one shared revision range and returns cursor-paged outcomes.
- `operation: {"kind":"symbol_log",...}` starts at `revision` (default `HEAD`)
  and uses Git line-history traversal for the resolved symbol range.

Historical paths are repository-relative, revisions are resolved before object
lookup, and blobs remain subject to the configured per-file byte limit.
Nested symbols use the same `{name,parent}` identity accepted by exact live
reads. A unique bare name also resolves, while multiple matches return typed
`symbol_ambiguous` instead of selecting by parser order. In `diff_symbols`, an
ambiguous endpoint remains a per-target `unavailable` result with
`ambiguous_base_symbol` or `ambiguous_head_symbol`, so other targets still
succeed. `diff_symbol` permits the file or symbol to be absent at one endpoint:
`before` or `after` is omitted, the unified diff contains the complete bounded
addition or deletion, and `semantic_change.kind` is `added` or `removed`. A
symbol absent at both endpoints remains a typed `symbol_not_found` error.
`max_tokens` defaults to 8,000 and applies to historical source or unified diff;
truncation is explicit through `result_complete`, `HistoricalSymbol.truncated`,
or `diff_truncated`. `max_results` defaults to 20 and is capped at 100 for
`symbol_log`. Symbol metadata includes the resolved 12-character revision,
complete line range, kind, parent, and full-content hash. `returned_end_line`
exists only when `content` is returned; metadata-only diff endpoints and symbol
logs omit that range instead of reporting line zero. Symbol diffs normalize only
their private comparison buffers, so parser slices do not manufacture a
whole-file no-newline marker and the returned content and hash remain unchanged.
This tool deliberately has no index consistency mode because Git objects are
immutable.

`diff_symbols` accepts 1–64 targets and returns at most 32 per page. Each target
contains `path` and `symbol`; supply `head_path` and `head_symbol` together to
classify an explicit move or rename. Results preserve request order and include
the original `request_index` plus one of `unchanged`, `added`, `removed`,
`renamed`, `modified`, `not_found`, or `unavailable`. Base/head commit metadata
is shared by the page. A continuation cursor binds the resolved revisions,
ordered normalized target pairings, and next offset, so changing any of them
returns `stale_cursor`.

The batch operation parses each distinct file once per endpoint and performs at
most seven Git subprocesses regardless of target count. It accepts at most 32
distinct paths per endpoint, 1 MiB per blob, 8 MiB of blobs per endpoint, 1,024
parsed symbols per endpoint, and 1 MiB of retained unified diff per page.
`max_tokens` is shared across all diffs. If `max_response_tokens` requires
fitting, LeanToken first preserves the largest ordered prefix of symbol status
records, then fills their diffs in request order; `incomplete_reason` identifies
the source-token, diff-byte, or final-response bound. The public Rust service
exposes this as `Services::history_diff_symbols`; the existing `HistoryOperation`
enum remains source-compatible.

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

For autonomous broad triage, make one materialized call with `plan_only=false`
and use the evidence directly. Make at most one focused follow-up only when the
returned coverage identifies a concrete missing implementation or
regression-test owner. This is a host usage contract, not service session state
or a restriction on implementation agents. The
[repeated multi-agent context suite](measurement.md#repeated-multi-agent-context-suite)
records the four-task, 60-run evidence and its limits.

Optional inputs focus or exclude paths and symbols, provide hashes already held
by the caller, and identify a prior repository generation. `include_paths` is a
hard boundary: every returned source fragment must match at least one supplied
pattern, while `focus_paths` remains a ranking boost unless
`strict_focus_paths=true`. `minimum_fragments_per_focus_path` reserves the
requested number of fragments for every focus pattern before ordinary ranking.
Context accepts at most 32 focus patterns and a minimum of at most eight
fragments per pattern. Required focus coverage receives bounded file-local
candidates before global top-N truncation; broad globs that exceed the
per-pattern file inspection bound report that limitation in `warnings`.
Explain-profile plans and materialized responses report a bounded allocation
diagnostic under each `coverage.focus_path_coverage[].diagnostics`. Balanced and
compact responses omit it without changing selection or making a metadata-only
plan larger than the corresponding materialized response. The diagnostic
separates eligible indexed paths, generated ranges and symbol ranges, enforced
reservations, exact selected source tokens, and non-zero suppression counts for
path policy, caller-held hashes, deduplication, source budget, fragment
capacity, per-file diversity, or soft global ranking. An unsatisfied path also
reports one `capacity_blocker`; it is an observed selection boundary, not a
task-success or relevance-confidence estimate. Counts are per pattern and are
not additive when focus globs overlap.
`strict_changed_paths=true` restricts fragments to the resolved explicit paths,
an immutable `BASE..HEAD` range, a base-revision-to-working-tree diff, or current
Git working-tree changes when neither diff input is supplied. Include, strict
focus, strict changed, and exclude constraints are intersected; no constraint
silently broadens another. `must_include_paths` and
`must_include_symbols` generate and select required indexed evidence before
focus minimums and ordinary ranking. A required path guarantees path
representation, not task relevance. Its candidate is the highest task-matching
bounded chunk in the selected file; when no task query matches, context returns
an explicit `required_path_fallback` excerpt from the start of the file.
`max_fragments` defaults to 8 and accepts values through 100.

Use `required_evidence` when path presence is insufficient. Each entry supplies
a `path`, one to sixteen literal `queries`, and an optional
`minimum_query_matches` (default one). Context selects matching excerpts before
ordinary ranking and reports each contract under `coverage.required_evidence`.
`evidence_scope_satisfied` is true only when every contract matches an indexed
path and selected or already-held evidence covers the requested number of
distinct queries. Up to 32 contracts and 64 KiB of query text are accepted.
Matches are case-insensitive literals and are materialized as bounded 40-line
excerpts centered on the evidence line. MCP accepts these objects directly;
the CLI accepts the same object as repeatable JSON, for example
`--required-evidence '{"path":"paper/**","queries":["claim boundary"]}'`.

`workflow_evidence` is an opt-in object for facts the caller directly observed
while executing the workflow. Its four arrays are `failure_traces`, `symbols`,
repository-relative `paths`, and `test_intents`. Each class accepts at most
eight items, each item accepts at most 8 KiB, and the combined payload accepts
at most 32 KiB. Evidence shares the existing 12-query context fan-out instead
of starting an unbounded second search. Do not populate it from benchmark gold
labels or guesses:

```json
{
  "task": "fix the failing default-value regression",
  "workflow_evidence": {
    "failure_traces": ["error: default_values_if is missing"],
    "symbols": ["default_values_if"],
    "paths": ["tests/builder/default_vals.rs"],
    "test_intents": ["default values regression"]
  }
}
```

Required symbols share the request token budget. A definition that fits its
share is returned completely. When it does not fit, the fragment and plan
candidate report `truncated: true` together with the complete
`target_start_line` and `target_end_line`; coverage reports the name under
`partial_must_include_symbols` instead of claiming complete coverage.

For human review or control-plane inspection before expensive or high-risk
materialization, set `plan_only=true` to run the same hard scopes, ranking,
must-cover selection, token budget, and fragment limit without returning
source. The response has an empty `fragments` array and no server-managed
receipt mutation; `plan` contains bounded paths and ranges, final scores and
reasons, exact source-token estimates, focus coverage, completeness, and a
generated-artifact warning. `receipt_id` is rejected in plan mode; use
`known_hashes` for stateless suppression that must apply to both preview and
materialization. After approval, repeat the same request with
`plan_only=false` to materialize those candidates against the selected index
consistency boundary.

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

An MCP server can also serve a bounded set of additional repositories when
they are explicitly approved in the primary repository configuration:

```toml
[repository_contexts.docs]
root = "../docs-repository"
```

Context names are request-only identifiers; retrieval calls never accept a
filesystem root. The primary workspace is selected when `repository_context`
is omitted, while an approved name selects the corresponding repository. A
maximum of eight additional contexts is accepted. Each context has its own
index, generation, cache, and admission state; unknown names fail closed, and
receipts remain bound to the selected repository identity and generation.

The `coverage` receipt distinguishes unmatched focus/include constraints,
covered requirements, indexed requirements blocked by path or budget limits,
and requirements absent from the index. Every focus path returns indexed and
selected fragment counts with an implicit minimum of one; strict or explicit
minimum requests contribute to `path_scope_satisfied`.
The field reports path coverage only; it does not claim task relevance. Explicit
`required_evidence` contracts instead contribute to
`evidence_scope_satisfied` and report matched and unmatched queries per path.
Strict changed-path requests return resolved and selected changed-path counts.
An empty strict scope therefore returns an explicit coverage failure rather
than unrelated evidence. Already-held matching hashes satisfy a must-cover or
evidence requirement without resending source.

`omission_summary` distinguishes path filtering, known hashes, and budget or
result limits with aggregate counts in both `compact` and `balanced` responses.
Choose the MCP `response_profile` field or CLI `--response-profile` flag:

- `compact` preserves fragments, receipts, hard-constraint coverage, warnings,
  aggregate omission counts, and retry routing, but removes individual
  omissions, verbose facets, and optional diff evidence.
- `balanced` is the default and preserves the historical non-verbose response.
- `explain` adds bounded individual omissions, path, language or file-type,
  reason, score-band, focus and changed-path facets, plus available diff
  evidence.

Every response reports `effective_response_profile`. Profiles do not change
candidate generation, ranking, fragment membership or order, source-token
budgets, hard constraints, or receipt suppression. They only change the
serialized presentation cost. The `--verbose-diagnostics` CLI option maps to
`explain`; combining it with an
explicit `compact` or `balanced` profile is rejected. Facet lists are
deterministic and bounded to 12 values; longer path or file-type tails are
combined into `[other]`. Candidates rejected before scoring use the `not scored`
band. The selector merges overlapping candidates, suppresses duplicate or known
content, preserves file diversity, and returns short reasons for each chosen
fragment.

A compact immutable review journey can first preview and then materialize the
same request:

```json
{
  "task": "review the change for correctness regressions",
  "workflow": "review",
  "base_revision": "origin/main..HEAD",
  "strict_changed_paths": true,
  "response_profile": "compact",
  "plan_only": true
}
```

Check `coverage.changed_path_coverage` and `path_scope_satisfied` for the hard
range boundary, and inspect `workflow_receipt.owner_test_candidates` plus
`missing_families` for bounded owner-test evidence and gaps. Then repeat it with
`plan_only=false`. Use `response_profile="explain"` only when the omitted or
focus-allocation or semantic diff diagnostics are needed.

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
`receipt_id` remains useful across processes while the same repository cache,
generation, and TTL survive; after a new generation, only the explicit
exact-only `receipt_rebase` operation can create a current receipt. Coordinates
and hashes remain the verification boundary. If Git identity or working-tree
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
leantoken --json context --task "fix the observed parser failure" \
  --failure-trace "error: unexpected token" --evidence-symbol Parser::parse \
  --evidence-path src/parser.rs --test-intent "parser regression"
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
- `tokenizer` identifies the tokenizer used for every count.

Accounting is filled repeatedly to a deterministic fixed point. Therefore
`source_tokens + protocol_tokens + path_and_metadata_tokens` equals
`total_response_tokens`, and tokenizing the final compact DTO produces that
exact total. Source limits remain independent and do not by themselves impose a
hard ceiling on the final serialization. All retrieval callers can opt into
that second boundary with `ServiceCallOptions`, MCP `max_response_tokens`, or
CLI `--max-response-tokens`; a mandatory correctness skeleton that cannot fit
returns a typed `ResponseBudgetExceeded` error. CLI JSON and MCP error data
include `provided_max_response_tokens`, `minimum_required_response_tokens`,
`retry_with_at_least`, and a bounded aggregate `breakdown`; the established
`requested` and `limit` fields remain available. Retrying with the reported
minimum is exact, while one token less remains insufficient. `files`, `read`,
history text/commit results,
context, and JSON keys pages use deterministic operation-aware fitting. Other
shapes fail loudly instead of dropping evidence without a valid continuation.
These counts and limits describe the service
response DTO, not MCP
text/structured-content duplication, tool schemas, provider framing, or JSON-RPC
envelopes. The `dual` troubleshooting mode serializes the structured payload in
both text and `structuredContent`; use the wire-cost harness when that boundary
matters.
The default tokenizer is `cl100k_base`. Exact built-in modes are `cl100k_base`,
`o200k_base`, `o200k_harmony`, `p50k_base`, `p50k_edit`, `r50k_base`, and
`gpt2`.

`estimate` is an inexact heuristic for providers whose tokenizer is not
available locally. It does not guarantee that a provider will accept a payload
at the reported budget; responses mark this with `token_count_exact: false`.

`savings` uses the same tokenizer and marks whether its local counts are exact.
The `response_accounting.estimated_net_tokens_saved` retrieval-compression
value subtracts every
recorded complete response from the represented-source baseline, so metadata,
protocol, plan-only, discovery, and history costs can reduce the net result.
It does not establish task-level savings. Only successful persisted responses
contribute token deltas. `observed_task_savings` therefore reports no task-level
percentage until host-linked outcomes and the declared cost categories are
available. It prominently separates observed failed calls and successful
responses with unknown relevance; failure-response tokens, retry chains,
superseded calls, and task outcomes remain explicitly unknown rather than being
treated as zero. `observations` retains the lower-level persisted failure and
exact expected-hash counters.

Source limits do not include JSON keys, paths, scores, hashes, receipts, tool
schemas, or JSON-RPC envelopes. `total_response_tokens` captures the compact response
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
top-level `error` string as an established error-wire field. The additive `category`
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
| `serialization_failure` | A response or persisted value could not be serialized |
| `response_accounting_invariant` | Response token accounting failed to converge |
| `cache_prune_failure` | Cache maintenance could not prune an artifact |
| `setup_failure` | Setup or installation state violated an invariant |
| `operation_failure` | A product operation reached an unexpected state |
| `internal_error` | An implementation, storage, I/O, or other unexpected failure |

The structured fields are an allowlist. I/O and SQLite failures use
`category: "internal_error"`; the typed failure categories above identify
other audited boundaries without exposing additional implementation details.
Future releases may add categories or optional fields, so consumers should
ignore keys and category values they do not know.

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
