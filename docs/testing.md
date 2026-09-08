# Testing architecture

## Coverage evidence

`cargo xtask coverage` instruments the same nextest product command, features,
package exclusion, CI profile and resource groups as `cargo test-product`.
`cargo xtask coverage --dry-run` prints it; append `--filterset 'test(pattern)'`
for focused diagnostic coverage. A filtered run cannot satisfy the full budget.
CI pins Rust 1.95 and cargo-llvm-cov 0.9.1 and runs coverage only on the events
allowed by `ci/test-topology.json`.

`ci/coverage-policy.json` assigns each measured production file to an owner and
ratchets line, function and region coverage. Each initial floor is five
percentage points below the observed integer percentage, with non-exception
line floors kept above 30%. The two low-line
coverage gaps have explicit review conditions; they remain in the report.
There are no production ignore patterns. Architecture checks reject stale
paths, invalid budgets and unexplained low-coverage entries. Reporting rejects
new unbudgeted files, missing required files and any failed file budget.
An independent module/source inventory also rejects files absent from both
policy and report. Exact declaration-only dispositions are visible separately;
syntax checks reject added functions, item macros and runtime closures there.
Budget changes require review and must explain missing transitions; aggregate
coverage cannot compensate for them.

The initial Linux product run on 2026-09-08 built in 65 seconds and executed
1,234 tests across six binaries in 77.124 seconds, all passing. The full JSON
was 15 MiB and the coverage build/profile tree was 3.5 GiB. It emitted no LLVM
profile warnings. Historical issue #511 records the prior workspace lane;
its timings/cache size came from a different host/revision and are not a
controlled speedup comparison. Benchmark binaries are now excluded by the
shared product graph. The obsolete ignored profiler was removed after this
initial measurement.

The final local gate passed all 1,246 tests across six binaries in 79.233
seconds, with no LLVM warnings or failed file budgets. Reported coverage was
89.25% lines, 88.78% functions and 87.34% regions across 58 owners, with 19
checked declaration-only files and two explicit reviewed gaps. The instrumented
test phase, including compilation, took 116.772 seconds. These percentages
describe execution evidence, not branch coverage or a correctness proof.

Reports under `target/coverage` retain the exact command, tool and topology
identities, before/after source fingerprints, per-owner metrics, raw JSON with
functions and uncovered segments, verbose object/merge commands,
phase timing/status and tool diagnostics. CI retains reports and raw profiles
and the instrumented nextest JUnit inventory for 30 days. Any report warning, including mismatched functions, invalidates
the run. Profile cleanup is limited to cargo-llvm-cov's raw profiles. Stable
branch coverage is unavailable and carries no correctness claim. The process
harness preserves `LLVM_PROFILE_FILE`; gracefully exiting children contribute
coverage, while forced termination may lose profile buffers. Process assertions
remain the authority for shutdown and failure composition.

See the [cargo-llvm-cov documentation](https://github.com/taiki-e/cargo-llvm-cov)
for nextest instrumentation and JSON export semantics.

## Behavioral ownership

Limit tables live in `services::request_limits`; static request tables live
beside the files, search, outline, read and context production parsers. These
tests construct scalar limits and DTOs, with no filesystem, database or runtime.
The services consume their parsed limits, paths, patterns and policy values.
The root fixture is named `indexed_fixture` at every call site to expose its
cold-index cost. Validation-only tests must use the parser seam; an indexed
validation owner must document its side-effect composition claim.

| Owner | Resource seam and reason |
| --- | --- |
| Request limit and static-input matrices | Pure production parsers; exhaustive bounds and cross-field input |
| `services::limits` | Five independent indexed fixtures: valid routing, tiny-budget evidence, limit/static-error side-effect ordering and generation ordering |
| MCP limit error contract | Open empty storage and real in-memory transport; no indexing, only wire error classes |
| MCP omitted limits | Indexed retrieval; configured-default propagation |

The limit migration removes four indexed service setups and one indexed MCP
setup. Five pure static-input matrices replace the exhaustive indexed replay;
one invalid request per operation retains generation and failure-accounting
composition evidence. The tiny-budget test remains indexed because candidate
presence determines its claim. Future resource changes should update this
inventory in review rather than add a source-name heuristic.

Linux measurements on 2026-09-08 compared baseline `464bda6fd8f5` with this
migration using the same product scheduler. The focused baseline contained nine
indexed limit tests; the replacement contained five indexed composition tests,
five pure limit tables and five pure static-input tables.

| Focused run | Baseline test wall / summed test time | Replacement test wall / summed test time |
| --- | --- | --- |
| First run, including fresh fixture setup | 2.207s / 7.485s | 0.745s / 2.311s |
| Warm build, fresh fixtures | 1.162s / 3.783s | 0.802s / 2.451s |

Including compilation, first command wall time was 44.18s versus 11.99s; warm
command wall time was 1.50s versus 1.19s. Dependency caches were shared, so the
first run is not a fully cold build comparison. Full product execution was
98.205s for 1,217 baseline tests and 98.024s for 1,244 replacement tests.
These are single shared-host observations, and a release build overlapped the
replacement full run; they do not establish a full-suite speedup. Each run
created fresh indexed fixtures. The scoped service fixture count fell from nine
to five, and the MCP limit-error fixture stopped indexing entirely.

The product may depend on private `leantoken-test-support` only as a Cargo
dev-dependency, so root integration tests can reuse hermetic Git setup.
Production and build dependencies on private test packages remain forbidden,
and test-support cannot depend back on product, suite or xtask packages.

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
cross-package ambiguity are errors. `plan --dry-run` performs no test work and
prints an explicitly named `local` or `ci` profile. The
contract benchmark is an explicit `test = false` example and is run only by
`cargo test-contract`; it is not an ignored default test. The product plan has
one Cargo build graph and one scheduler spanning library and binary units,
private domains, ordinary integration, and executable or MCP process behavior.
JUnit suites and module-qualified test names preserve owner timing without
starting a second scheduler. Checked-in corpora and generated reports run
through the domain, contract, or benchmark target that owns their meaning;
there is no generic fixture runner or blessing path.

`cargo xtask test stress` runs its explicit process-lifecycle command once by
default. Scheduled jobs set `LEANTOKEN_STRESS_REPETITIONS` to their
platform-specific repetition count, bounded from 1 through 100. Multi-run
invocations preserve each bounded JUnit report under
`target/nextest/stress/repetitions/`; these are deliberate repeated evidence
runs, not retries of failed merge tests. The runner clears the current JUnit
file before every repetition and rejects symlinked or reparse-point ancestors,
so each preserved report is fresh evidence rooted in the workspace.

`cargo xtask test profile` is the weekly timing lane and selects the named
`profile` policy. It continues after a failure, writes complete JUnit metadata,
and prints slow tests and final failures. The `stress` profile similarly owns
its process-only repetition and output policy. Neither mode turns a retry into
merge evidence.

The deterministic product run uses `cargo-nextest` once with the complete
feature graph and a global platform bound. Six checked resource groups classify
cheap, cold-index/SQLite, Git, filesystem/watcher, real-process/MCP, and
extended tests; their semaphores, scheduler-slot reservations, and per-owner
timeouts are inherited by every named profile. Doctests remain a separate Cargo
command. Required lanes use zero retries, while scheduled stress and profiling
are separate lifecycle evidence rather than recovery for a failed merge test.

CI selection is produced by the checked-in `xtask` planner and
[`ci/test-topology.json`](../ci/test-topology.json). It records the event,
source revision, topology digest, selected and intentionally unselected lanes,
dependency edges, bounded executable jobs, and human-readable reasons. Each
lane declares `allowed_events` separately from the `required_events` that make
it mandatory without a path match. Every executable job carries its lane,
runner, command class, source revision, topology digest, bounded command
parameters, and deterministic receipt identity:

```bash
cargo xtask ci plan --event pull_request --base BASE --head HEAD \
  --changed-paths-file changed-paths.txt --dry-run
cargo xtask ci validate-plan --input target/ci-plan.json
cargo xtask ci validate-receipts --plan target/ci-plan.json \
  --receipts target/ci-receipts
```

Unknown paths, unavailable pull-request or merge-group bases, fork inputs, and
planner inconsistencies select the conservative evidence set and record a
fallback reason, but still cannot select a lane on an event it does not allow.
`--full-run` and `--diagnostic` only add event-eligible lanes. GitHub Actions
consumes the planner's job list directly; it does not reconstruct an OS matrix
from lane booleans. Each matrix entry uploads an identity-bound result receipt,
and the stable `Required checks` aggregate validates the complete receipt set
for pull requests and merge queues. A selected job that fails, cancels, times
out, changes identity, or disappears is not treated as a successful skip.

Pull requests run the fast Linux product owner for product and process-test
changes. Cross-platform product, token-economy, example, coverage, profile, and
stress evidence remains explicit in the merge, main, scheduled, or manual
events declared by the topology. All merge and CI Cargo commands use
`--locked`. Dependency updates are the
only workflow that intentionally changes `Cargo.lock`.

## Hermetic setup

Git fixture setup in the service and repository domains uses
`leantoken_test_support::GitFixture`. Initialization selects `main`, configures
a local test identity, and keeps LF bytes unchanged. Every setup and observation
command checks its exit status, retains at most 64 KiB from each output stream,
and has a ten-second deadline. The runner terminates its process group (Windows
job object), including descendants that retain output pipes. A separate temporary
home beside the fixture isolates global/system configuration, templates, hooks,
signing, pagers, prompts, and inherited Git repository routing. Tests may still
write local Git configuration explicitly to exercise the production Git path;
the setup runner does not replace that path.

Run `cargo test --locked -p leantoken-test-support --lib git::tests` to verify the
fixture runner. Test identities continue to select the existing `git-fixtures`
nextest resource group. Failure diagnostics are captured by the owning test's
normal output/JUnit reporting; collecting fixtures after a forced test-process
termination remains outside this helper's contract.

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

The MCP task supervisor and post-index retry wait are tested under paused Tokio
time, including terminal errors, cancellation, the retry boundary, and both
transport-first and runtime-first shutdown. Synchronous SQLite startup uses a
private retry owner whose wait operation can advance the test's recorded delay
sequence without sleeping. The real-process startup witness holds a real SQLite
lock until the existing contention warning confirms a failed open has entered
retry. Process tests retain failed-state, catalog, and EOF checks without waiting
through the production retry/shutdown intervals. Captured process stderr retains
the first 64 KiB and can be inspected while the child is alive; deadline failures
report the observed child state and captured diagnostics.

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
ignored tests. The concurrency profiler is an explicit benchmark binary linking
the ordinary library; capacity, cancellation and snapshot cleanup remain in
deterministic tests. The compiled ignored-test inventory must be empty.
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
