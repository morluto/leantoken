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

The process owner is decomposed by semantic boundary under `tests/process/`:
`cli.rs` owns CLI behavior, `doctor.rs` owns doctor and registration probes,
`mcp_protocol.rs` owns wire and receipt behavior, `mcp_lifecycle.rs` owns
startup, readiness, contention, and failover, `repository_free.rs` owns
repository-independent commands, and `runtime.rs` owns private-runtime setup
and cache lifecycle behavior. `support.rs` contains only the shared process
capabilities (hermetic launch, MCP transport, bounded readiness, and fixture
builders). The root `tests/process.rs` remains a thin test-owner registry so
the target and its stable test identities do not change when a semantic module
moves.

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
cargo xtask test stress
cargo xtask test profile
```

`xtask` prints every Cargo command before execution and preserves its exit
status. Focused selectors for named suite domains build only the owning suite;
other filters search both product and suite packages. Zero matches and
cross-package ambiguity are errors. `plan --dry-run` performs no test work. The
contract benchmark is an explicit `test = false` example and is run only by
`cargo test-contract`; it is not an ignored default test. The product plan has
three visible owners: library and binary units, ordinary integration, and
executable or MCP process behavior. Checked-in corpora and generated reports
run through the domain, contract, or benchmark target that owns their meaning;
there is no generic fixture runner or blessing path.

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

`Sandbox` creates one uniquely named tree under `target/test-sandboxes/` and a
repository directory inside it. Tests create only the additional files and
directories their scenario needs. On success the sandbox is removed. On panic,
or when `LEANTOKEN_TEST_KEEP=1` is set, the tree moves to
`target/test-failures/` and prints its focused rerun command.

Real-binary tests keep their process-specific setup in `tests/process/support`:
that owner supplies hermetic environment construction, bounded streams,
readiness checks, and shutdown behavior. These capabilities are not exported
from the general test-support crate until another semantic owner needs them.

## Fixtures and snapshots

Checked-in data stays with its semantic verifier. `fixtures/sample_repo` is the
small multilingual corpus shared by contract and representation tests.
Benchmark fixtures live under `benchmarks/fixtures`; their owning executable
parses the inputs, recomputes derived reports where applicable, and verifies
embedded digests. Behaviors that need only a small request and expected value
remain ordinary named Rust tests in the owning domain instead of serialized
case directories.

Snapshots are limited to stable external contracts such as CLI help, MCP
contracts, migrations, and intentionally versioned JSON. The full MCP contract
has one canonical snapshot rather than a second catalog-only dump. When output
ordering is not contractual, compare normalized records or multisets; do not
globally sort output merely to make a snapshot pass.

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
