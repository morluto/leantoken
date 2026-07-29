# Testing architecture

LeanToken's tests are organized by the invariant they prove and the resource
boundary they exercise. The product crate owns private unit tests. The private
`leantoken-test-suite` package owns cross-component domain tests, and the root
package owns the single process integration executable so Cargo can provide
`CARGO_BIN_EXE_leantoken` to tests that launch the real binary. The independent
`leantoken-test-support` package contains setup capabilities only; it never
depends on LeanToken.

## Layers and owners

| Layer | Owner | Evidence |
| --- | --- | --- |
| Colocated unit | Product modules | Pure parsing, ranking, accounting, cursors, limits, and private state transitions |
| Domain integration | `leantoken-test-suite` | Public behavior spanning storage, indexing, retrieval, protocol, platform, and contract boundaries |
| Process integration | Root `integration` target | CLI, stdio MCP, startup, shutdown, contention, and failover through the actual binary |
| Fast contracts | Contract-owned domain tests and explicit targets | Catalogs, envelopes, migrations, report shapes, and stable snapshots |
| Extended | Explicit executable targets | The token-economy benchmark and future exhaustive or long-running cases |

`tests/integration.rs` contains the one root integration target. Indexing,
storage, retrieval, protocol, contract, and platform owners live in the private
suite; CLI parser checks, service composition, reports, and real-binary process
behavior remain at the root where their seams are owned. Each owner carries its
fixtures and assertions; no forwarding copies exist.

## Commands

The contributor-facing aliases are locked and remain short:

```text
cargo test-focused services::search
cargo test-product
cargo test-contract
cargo test-extras
cargo xtask check-test-architecture
cargo xtask test plan --dry-run
cargo xtask test list [domain]
cargo xtask test run <domain>/<case>
cargo xtask test bless <domain>/<case>
cargo xtask test stress
cargo xtask test profile
```

`xtask` prints every Cargo command before execution and preserves its exit
status. `plan --dry-run` performs no test work. The contract benchmark is an
explicit `test = false` example and is run only by `cargo test-contract`; it is
not an ignored default test. Domain fixture execution and blessing are owned
by the domain module selected by the case operation. A generic runner never
rewrites expected output.

`cargo xtask test stress` runs its explicit process-lifecycle command once by
default. Scheduled jobs set `LEANTOKEN_STRESS_REPETITIONS` to their
platform-specific repetition count; these are deliberate repeated evidence
runs, not retries of failed merge tests.

`cargo xtask test profile` is the weekly timing lane. `.config/nextest.toml`
marks tests slower than ten seconds, terminates a hung test after six periods,
and sets retries to zero. The command prints slow tests and final failures;
profiling never turns a retry into merge evidence.

All merge and CI Cargo commands use `--locked`. Dependency updates are the
only workflow that intentionally changes `Cargo.lock`.

## Hermetic setup

Tests request only the capability they need:

- `Sandbox` owns repository, cache, home, configuration, log, and artifact
  directories. It strips inherited home, Cargo, Rustup, Git, pager, editor,
  locale, and prompt state from child-process environments.
- `RepoBuilder` creates typed text, binary, directory, symlink, and nested
  worktree fixtures while rejecting paths that escape the repository root.
- `GitRepository` initializes local Git state with a deterministic identity and
  disables system and global configuration.
- `ProcessHarness` launches an explicitly supplied binary with bounded stdin,
  stdout, stderr, timeout, and shutdown behavior.
- `FixtureCase` validates `schema = 1`, a domain operation, `request.json`,
  `expected.json`, and the allowed fixture layout.
- `Normalizer` makes named path, time, protocol, and platform substitutions.
  It does not normalize semantic counts, statuses, ordering, completeness,
  evidence identities, or token accounting.
- `Deadline` is for external readiness and reports the last observed state. It
  never establishes ordering between in-process participants.

On success a sandbox is removed. On panic, or when
`LEANTOKEN_TEST_KEEP=1` is set, the complete tree is preserved under
`target/test-failures/` and the harness prints its stable identifier and exact
focused rerun command when one is available. Process transcripts are stored
under the sandbox log directory.

## Fixtures and snapshots

Repeated cases use this layout:

```text
fixtures/<domain>/<case>/
├── case.toml
├── repo/
├── request.json
└── expected.json
```

`case.toml` contains only `schema = 1` and the domain-owned `operation`.
Requests and expectations are typed by that domain; there is no universal
field bag. `list` rejects malformed manifests, missing files, unknown contract
files, and duplicate identities. Blessing is exact-case only, never runs in
CI, and must leave a reviewable semantic diff.

Snapshots are limited to stable external contracts such as CLI help, MCP
catalogs, migrations, and intentionally versioned JSON. When output ordering
is not contractual, compare normalized records or multisets; do not globally
sort output merely to make a snapshot pass.

## Time, concurrency, and boundaries

Pure schedulers receive an explicit `now` value. Timer-only Tokio tests use a
current-thread paused clock and explicit advancement. Filesystem, SQLite,
watcher, and process tests use observable readiness plus a final deadline;
polling reports the last state and never uses sleep to establish ordering.

Every concurrency test states its invariant, participant and queue bounds,
start synchronization, cancellation owner, committed-state expectation, and
failure diagnostics. Internal hooks remain typed and owner-local. At least one
integration or process test proves every externally important transition.

Use the lowest sufficient seam: parser invariants stay unit-local, SQL
invariants use real SQLite, CLI contracts launch the binary, and watcher claims
use the native watcher. Do not initialize or index a repository when a lower
boundary proves the behavior.

## CI lanes

The required matrix runs units, ordinary domains, process behavior, and fast
contracts on Ubuntu, macOS, and Windows with `fail-fast: false`. Linux quality
also checks formatting, workspace clippy, architecture direction, and rustdoc.
Coverage and examples remain separate visible jobs. Extended tokenizer,
long-contract, repeated concurrency, profiling, benchmark, and model-evidence
work are explicit nightly, weekly, or manual lanes rather than permanently
ignored tests. The existing private-diagnostics concurrency profiler remains a
single migration exception because extracting it would require exposing
production-internal `Services` state; its release-only command is documented
in `docs/measurement.md` and it is not part of ordinary behavior evidence.
Failed matrix jobs upload `target/test-failures` with the OS and commit SHA in
the artifact name.

## Decomposition constitution

New or moved modules have one behavioral owner, one resource owner, directed
dependencies, and a deep interface. Support code is capability-based rather
than a prelude or utility bag. Real resource seams are retained, public APIs
are not widened just for tests, behavior-affecting defaults are explicit, and
temporary forwarding or dual-run scaffolding is deleted with its migration.
Another workspace crate is justified only when it improves dependency
enforcement, compilation ownership, or independent execution enough to offset
another compilation unit.
