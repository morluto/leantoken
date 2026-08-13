# Monorepo reconciliation decision

Date: 2026-07-20

Experiment: `monorepo-reconciliation-v1`

Workflow: [GitHub Actions run 29760207560](https://github.com/morluto/leantoken/actions/runs/29760207560)

LeanToken ran from Git commit `b5879d2fe826102d680036cef1613ae0e8c9f4dd`.
GitHub checked out PR merge commit
`829b641566cfe00ff50da18e500678c86f594dbb` for the measurement.

The measured merge revision and LeanToken head have the same Git tree,
`2ac6379e0c54ec6b8eacbe5a4123c7883c6c7c65`. The merge revision is recorded
because GitHub Actions checked out the pull-request merge ref. All six raw jobs
and the strict aggregate job completed successfully.

## Frozen decision rule

The manifest was committed before the matrix ran. A changed-path journal or
directory invalidation prototype was eligible only if:

1. full fallback reached at least 250 ms p95;
2. discovery plus hash/planning consumed at least 50% of mean full-noop time;
3. both conditions held in at least two corpus/platform pairs.

The profile used 10 iterations, one read sample, a one-file hot set, and a 50 ms
watcher debounce. It ran release binaries from clean worktrees against Tokio
`9cae638de6dc8dd9779c450201df8c102247a242` and Vue core
`31d0f23757afb410c638a9c29d44d76d0944e18f` on GitHub-hosted Linux, macOS,
and Windows runners.

## Results

| Platform | Corpus | Files | Initial index (s) | Full no-op p50 / p95 (ms) | Scan share | Targeted modify p95 (ms) | 30-file directory rename p95 (ms) | Watcher p95 (ms) | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Linux | Tokio | 865 | 6.23 | 14.38 / 15.62 | 99.69% | 2.84 | 331.40 | 51.86 | 67.16 |
| macOS | Tokio | 865 | 5.95 | 24.22 / 29.01 | 99.67% | 1.81 | 298.60 | 67.41 | 68.45 |
| Windows | Tokio | 865 | 10.41 | 43.22 / 51.89 | 99.72% | 60.61 | 1,790.14 | 57.10 | 57.35 |
| Linux | Vue core | 703 | 16.05 | 12.48 / 13.74 | 98.29% | 14.05 | 2,304.62 | 51.58 | 83.86 |
| macOS | Vue core | 703 | 12.45 | 24.80 / 32.45 | 98.46% | 11.49 | 2,728.42 | 72.17 | 85.48 |
| Windows | Vue core | 703 | 19.11 | 41.12 / 46.89 | 99.02% | 59.20 | 4,886.09 | 60.72 | 73.85 |

All native watcher samples produced one changed-path delivery after the quiet
period: 10 changed messages and zero full-reconciliation messages per pair.
The final SQLite/WAL/SHM logical footprint ranged from 66.79 to 68.14 MiB for
Tokio and from 77.80 to 81.60 MiB for Vue core. Windows checked out the same
commits with platform line endings, so its visible byte totals differ while
file counts and revisions remain fixed.

Hash/planning was the dominant full-noop phase in every pair. The high scan
share is not material by itself: every full-noop p95 was between 13.74 and
51.89 ms, at most 21% of the frozen absolute threshold. Therefore zero of six
pairs qualified. The strict aggregate decision was:

> no-go: 0 corpus/platform pairs met the frozen materiality threshold; retain
> targeted reconciliation with bounded full fallback

The directory-rename timings do not overturn that result. They include parsing
and publishing the moved files plus affected importers, not just rediscovering
metadata. They motivate keeping targeted directory handling bounded and
correct, but do not isolate a journal-addressable full-scan bottleneck.

## Correctness evidence

Portable overflow and interruption performance events were not forced in the
matrix. Their safety properties remain deterministic gates:

- watcher input overflow becomes one sticky full reconciliation;
- a full output queue degrades changed paths to full reconciliation;
- startup bursts collapse to one quiet reconciliation and repeated fallback is
  rate-limited;
- streamed cancellation, storage failure, and panic roll back uncommitted work;
- a follower rebuilds after the elected leader is killed during reconciliation.

Absolute timing and RSS values are runner-specific. This two-corpus matrix does
not represent cold disks, network filesystems, antivirus configurations, or all
repository shapes. Reconsider the no-go only with a newly frozen experiment
showing a material full-fallback cost on target workloads.
