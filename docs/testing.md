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
cargo test-focused protocol
cargo test-product
cargo test-contract
cargo test-extras
cargo xtask check-test-architecture
cargo xtask test plan --dry-run
cargo xtask test fixtures
cargo xtask test list [domain]
cargo xtask test run <domain>/<case>
cargo xtask test bless <domain>/<case>
cargo xtask test stress
cargo xtask test profile
```

`xtask` prints every Cargo command before execution and preserves its exit
status. Focused selectors for named suite domains build only the owning suite;
other filters search both product and suite packages. Zero matches and
cross-package ambiguity are errors. `plan --dry-run` performs no test work. The
contract benchmark is an explicit `test = false` example and is run only by
`cargo test-contract`; it is not an ignored default test. Domain fixture
execution and blessing are owned by the domain module selected by the case
operation. Required fixture evidence runs as one test-profile aggregate, while
exact `run` and `bless` operations retain the standalone runner. This avoids a
second development-profile product build after unit tests. The unit phase
builds the fixture-runner test harness but skips its aggregate; a separate
exact phase uses the same workspace feature graph and runs it after the
parallel suite-lib harness. A generic runner never rewrites expected output.

`cargo xtask test stress` runs its explicit process-lifecycle command once by
default. Scheduled jobs set `LEANTOKEN_STRESS_REPETITIONS` to their
platform-specific repetition count; these are deliberate repeated evidence
runs, not retries of failed merge tests.

`cargo xtask test profile` is the weekly timing lane. `.config/nextest.toml`
marks tests slower than ten seconds, terminates a hung test after six periods,
and sets retries to zero. The command prints slow tests and final failures;
profiling never turns a retry into merge evidence.

The deterministic product phases use `cargo-nextest` with the same feature
graph and explicit process bounds. Doctests remain a separate Cargo command;
nextest does not silently replace documentation evidence. Required lanes use
zero retries, while scheduled stress and profiling are separate lifecycle
evidence rather than recovery for a failed merge test.

CI selection is produced by the checked-in `xtask` planner and
[`ci/test-topology.json`](../ci/test-topology.json). It records the event,
source revision, topology digest, selected and intentionally unselected lanes,
dependency edges, bounded matrices, and human-readable reasons:

```bash
cargo xtask ci plan --event pull_request --base BASE --head HEAD \
  --changed-paths-file changed-paths.txt --dry-run
cargo xtask ci validate-plan --input target/ci-plan.json
```

Unknown paths, unavailable pull-request or merge-group bases, fork inputs, and
planner inconsistencies select the conservative evidence set and record a
fallback reason. `--full-run` and `--diagnostic` only add lanes. The stable
`Required checks` aggregate runs for both pull requests and merge queues; a
selected job that fails, cancels, times out, or disappears is not treated as a
successful skip. Branch protection must require that aggregate before PR
platform coverage is narrowed.

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
files, duplicate identities, more than 10,000 scanned directory entries, and
trees deeper than 64 levels. Listing accepts only `<domain>/<case>` directories,
rejects case directories without a manifest, excludes the `sample_repo`
benchmark corpus before bounded traversal, and does not follow directory
symlinks. The xtask preflight and fixture test harness use this same inventory
source module; xtask includes its std-only source without adding a dependency
on a workspace product or test package.
Blessing is exact-case only, never runs in CI, and must leave a reviewable
semantic diff.

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

The planner selects units, ordinary domains, process behavior, and fast
contracts independently from their owned paths. Selected product and contract
lanes run on Ubuntu, macOS, and Windows with `fail-fast: false`. Linux quality
also checks formatting, workspace clippy, architecture direction, and rustdoc.
Coverage and examples remain separate visible jobs and are not enabled merely
because another Rust-owned lane changed. Extended tokenizer,
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
