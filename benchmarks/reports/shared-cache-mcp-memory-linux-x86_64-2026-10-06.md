# Shared-cache MCP memory scope (Linux x86_64, 2026-10-06)

This characterization measures native MCP process memory for #604. Shared SQLite caches elect one indexing leader and watcher; they do not share the tokenizer or all process state. No production memory optimization is adopted. The repaired schema-v5 matrix has complete files-pagination parity, but its warm-latency screen still fails under the one-CPU quota.

## Source and measurement identity

- Clean locked release-profile CLI source [`39efa0cbfdcf7fed9d6050b8b3c9142c49a28351`](https://github.com/morluto/leantoken/commit/39efa0cbfdcf7fed9d6050b8b3c9142c49a28351), SHA-256 `e2d1bfb8b8fd64e6567c283d05f83d52559a4a25fc5115c54b223f236b086911`. This is a source build, not a published release. Product sources match the profiler revision.
- Clean locked release profiler source [`b23728ca8e00c99bd968ef0840f6506921555aa1`](https://github.com/morluto/leantoken/commit/b23728ca8e00c99bd968ef0840f6506921555aa1), SHA-256 `8af743da46dd8843f1d6269046d63c4bf5c46d450df1656c75de0e752e290bbf`; build took 24.91s.
- Isolated Linux container: one CPU, three GiB memory, one build/index worker, separate HOME/cache, no network; heavy builds and probes run serially. Host MCP version/configuration remain unchanged. Advertised logical CPU count in the raw report does not override the container quota.
- Fixture: 200 Rust files × 40 functions. Process counts 1/4/8, shared/independent caches in ABBA order, ten warm rounds, nine-second idle windows; dedicated 50,001-directory polling fixture observed for 31 seconds.
- [Exact parsed native matrix and activation observations](shared-cache-mcp-memory-linux-x86_64-2026-10-06.json). Matrix raw SHA-256 `b4b198dd0120526b53202439fa82ae607f98791c53ec9bc838d8fcc4ef7fe5ff`; activation raw SHA-256 `e8cd89d910037d2a9ad2ee1571470c73da0704b8f86b59d4c3b43010996110ae`.

## Memory observations

Each cell is the min–max of the two ABBA arms, in MiB. Status RSS is `/proc/<pid>/status` VmRSS; rollup RSS, proportional set size (PSS), and private resident pages are sampled together from `smaps_rollup` outside timed query/idle CPU windows. Processes are sampled sequentially; the cohort is not one atomic snapshot. Aggregate values require every cohort member and checked sums. PSS apportions shared pages; private resident pages include more than heap allocations. These metrics cannot independently identify an allocator or pool owner.

| Processes | Cache topology | Status RSS | Rollup RSS | PSS | Private resident |
| --- | --- | --- | --- | --- | --- |
| 1 | shared cache | 51.91–53.22 | 54.49–55.45 | 53.62–54.64 | 52.74–53.82 |
| 1 | independent caches | 52.41–53.18 | 54.58–55.55 | 53.72–54.70 | 52.86–53.86 |
| 4 | shared cache | 184.85–186.70 | 193.45–194.80 | 135.06–136.83 | 115.25–116.94 |
| 4 | independent caches | 209.70–209.73 | 218.27–218.76 | 155.07–155.76 | 133.87–134.53 |
| 8 | shared cache | 361.40–364.93 | 377.92–380.67 | 242.89–245.95 | 222.45–225.72 |
| 8 | independent caches | 420.00–425.38 | 437.18–442.27 | 289.62–294.82 | 267.51–272.74 |

Arithmetic increments from ABBA cohort means relative to the one-process shared baseline:

- 4 processes: status RSS 44.40 MiB, PSS 27.27 MiB, private resident 20.94 MiB per added follower.
- 8 processes: status RSS 44.37 MiB, PSS 27.19 MiB, private resident 24.40 MiB per added follower.

These increments are descriptive, not paired estimates of marginal host physical memory or heap ownership. All shared arms have exactly one leader/watcher. Each process reports four estimated read connections; capacity alone does not establish their retained allocations.

## Empty-root tokenizer activation control

Four fresh native servers, empty indexed roots, fixed `cl100k_base / estimate / estimate / cl100k_base` order, first files query then ten repeats. This isolates a substantial first-query retention component without source/index payloads. It does not assign all retained pages to the tokenizer constructor.

| Arm | Tokenizer | First-query PSS growth (KiB) | First-query private growth (KiB) | Ten-repeat PSS growth (KiB) | First query (ms) |
| --- | --- | --- | --- | --- | --- |
| 0 | cl100k_base | 24660 | 24660 | 204 | 92.87 |
| 1 | estimate | 832 | 832 | 68 | 6.06 |
| 2 | estimate | 896 | 896 | 76 | 5.99 |
| 3 | cl100k_base | 24428 | 24428 | 32 | 102.27 |

Exact BPE activation grows PSS/private by roughly 24 MiB in both arms. The locked tiktoken-rs 0.12.0 constructor retains process-local lookup structures and its singleton is process-local; this supports an ownership hypothesis, not a complete heap profile. Estimate mode reports `token_count_exact=false` and is ineligible as an exact-accounting optimization. All four arms retain generation one and close with exit zero and no unanswered IDs. Short windows and ten repeats cannot establish or exclude a long-lived leak.

## Correctness and performance limits

- Schema v5: zero of 2,284 parity checks mismatch. Every paginated baseline validates the complete ordered 200-file inventory through its own session and replays its first continuation; 1,040 extra validation calls occur outside warm timing. Unknown or changed warm cursors remain significant.
- Twelve arms / 52 matrix children and one polling child: four intentional takeover kills, 49 checked normal EOF paths, all ownership/takeover bounds and all 36 memory aggregate checks pass.
- Expected response-accounting denominators include validation (65 calls/process). Shared-cache accounting is best-effort and loses some concurrent updates: 238/260, 242/260, 462/520, 458/520 in four arms. Other arms match. This is not an independent complete-wire BPE oracle.
- Decision remains `investigate_host_wide_admission`: warm p95 ratio 11.7504 exceeds the unchanged 3.0 threshold. Startup ratio 1.8073, cold CPU/repository ratio 1.3057 and eight-process CPU/query ratio 1.0852 do not constitute a passing overall gate. Differences from earlier invalid matrices are unpaired and include changed validation calls/cache history; do not claim faster production.
- Eleven nine-second matrix idle windows register zero CPU ticks; one registers 10 CPU-ms. Zero measured ticks does not mean zero work. Dedicated admitted-tree polling registers one reconciliation and 1,700 CPU-ms in 31,001.314 wall-ms (5.4836% of one core).
- No pool resizing, approximate tokenizer substitution, shared-service deployment, scan-interval change or host reconfiguration is adopted. Allocation attribution and any bounded reduction still require their own correctness, latency/backpressure and paired resource evidence.

## Reproduction

Build the CLI/profiler from the exact linked revisions with locked dependencies. Run the profiler serially in the stated quota with:

```bash
mcp_multiprocess_profile --binary /path/to/pinned/leantoken \
  --max-index-workers 1 --process-counts 1,4,8 \
  --files 200 --functions-per-file 40 --warm-iterations 10 \
  --idle-seconds 9 --polling-directories 50001 \
  --polling-observation-seconds 31 --timeout-seconds 30 \
  --output memory-profile.json
```

The activation observations use empty disposable roots: initialize MCP, query files tree once, repeat ten times, sample status and bounded smaps_rollup reads, then close stdin and verify zero exit/no unanswered IDs. Run the four configured tokenizer arms in the fixed order above; no other compiler or native query process overlaps sampling.

Related: #604, #603, #634, #635/#637 and #636/#638. The older 31-mismatch schema-v4 matrix remains preserved; this report does not rewrite its invalid decision.
