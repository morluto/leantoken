# Live-read cache decision

Date: 2026-07-20

Experiment: `live-read-decision-v1`

Workflow: [GitHub Actions run 29762380229](https://github.com/morluto/leantoken/actions/runs/29762380229)

LeanToken ran from Git commit `87066385d0a150e629571cae116dcc47c8cdbfde`.
All six native profile jobs and the strict aggregate job completed successfully.

## Frozen decision rule

The manifest was committed before the matrix ran. A 64 MiB byte-bounded LRU
prototype was eligible only if:

1. direct whole-file reads reached at least 1 ms p95;
2. direct reads consumed at least 10% of mean `Services::read` plus JSON time;
3. both conditions held in at least two corpus/platform pairs.

The decision treats every microsecond of direct filesystem time as avoidable,
even though a real cache would still pay for lookup, synchronization, cloning,
eviction, invalidation, and memory ownership. It therefore favors a prototype.
The profile used 200 samples, an eight-file repeated working set, a 512-token
response limit, and a retained and page-touched 256 MiB pressure buffer. It ran
against Flask `2ac89889f4cc330eabd50f295dcef02828522c69` and Vue core
`31d0f23757afb410c638a9c29d44d76d0944e18f` on GitHub-hosted Linux, macOS,
and Windows runners.

## Results

| Platform | Corpus | Warm direct p95 (ms) | Pressure direct p95 (ms) | Warm / pressure share | Warm / pressure complete p95 (ms) | Spread complete p95 (ms) | Peak RSS (MiB) | Material |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Linux | Flask | 0.0018 | 0.0020 | 0.21% / 0.26% | 3.32 / 3.12 | 5.11 | 308.2 | no |
| macOS | Flask | 0.0493 | 0.0723 | 1.18% / 1.97% | 8.75 / 8.23 | 14.07 | 309.7 | no |
| Windows | Flask | 0.0364 | 0.0369 | 1.41% / 1.45% | 6.45 / 6.49 | 9.70 | 298.5 | no |
| Linux | Vue core | 0.0071 | 0.0059 | 0.15% / 0.15% | 6.43 / 5.61 | 12.54 | 328.3 | no |
| macOS | Vue core | 0.0128 | 0.0222 | 0.23% / 0.35% | 8.44 / 6.85 | 14.55 | 330.0 | no |
| Windows | Vue core | 0.0327 | 0.0353 | 0.46% / 0.47% | 8.07 / 8.00 | 17.49 | 312.3 | no |

The post-index first-pass direct p95 range was 0.004-0.089 ms. In-memory clone
p95 was 0.00008-0.00040 ms, but the decision did not subtract it or charge any
cache overhead. The selected eight-file payload was only 15.8 KiB for Flask and
43.1-44.8 KiB for Vue core, so this is already a favorable repeated working set
well below the proposed 64 MiB capacity.

Peak RSS includes the intentionally retained 256 MiB pressure buffer and ranged
from 298.5 to 330.0 MiB. Initial indexing ranged from 0.86 to 1.96 seconds for
Flask and from 9.80 to 19.70 seconds for Vue core; indexing is reported
separately and is not part of the live-read cache threshold.

Every pair also passed the same live-change contract. A changed body was read
immediately with `index_stale=true` while generation 1 remained committed;
targeted reconciliation then published generation 2 and the next read was
current. A process-local body cache would have to preserve this behavior across
watcher delay, explicit working-tree consistency, and multiple processes.

## Decision

The most cache-favorable pair reached only 0.0723 ms avoidable p95 and 1.97%
mean local request share. These are respectively 7.2% and 19.7% of the frozen
thresholds, and no pair met either threshold. The strict aggregate decision was:

> no-cache: 0 corpus/platform pairs met the frozen live-read threshold; retain
> bounded live filesystem reads and the operating-system page cache

No LRU prototype or runtime cache state was added.

## Limitations

The checkout copy and initial index touch every corpus file, so the first
profile pass is not evidence of a cold operating-system page cache. The touched
pressure buffer proves retained process memory, but cannot force or verify
equivalent page-cache eviction across operating systems. No provider or model
runs in this profile; comparing direct reads only with local service and JSON
time makes their share an upper bound for an agent request.

GitHub-hosted local filesystems do not represent remote, encrypted,
antivirus-heavy, or contended deployments. A deployment with those properties
may cross the threshold, but requires a new in-situ frozen experiment before a
scoped cache decision. It is not evidence for a default process-local cache.
