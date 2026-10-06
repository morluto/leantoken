# Tokenizer ownership screens (Linux x86_64, 2026-10-06)

Both ownership prototypes for #604 are rejected. Sharing all tokenizer lookup bytes reduces resident memory materially, but increases CPU/query and fails latency gates. Preserving the hot encoder layout in a smaller prototype passes semantic controls but misses the predeclared allocation floor, so its expensive native build and matrices are not run. Neither dependency fork is adopted.

## Scope and identity

- The original release CLI is built from [`39efa0cbfdcf7fed9d6050b8b3c9142c49a28351`](https://github.com/morluto/leantoken/commit/39efa0cbfdcf7fed9d6050b8b3c9142c49a28351), SHA-256 `e2d1bfb8b8fd64e6567c283d05f83d52559a4a25fc5115c54b223f236b086911`. Product `src`, Cargo.toml and Cargo.lock match candidate base [`988bb0830c841f20470f6ee8da0b041812832762`](https://github.com/morluto/leantoken/commit/988bb0830c841f20470f6ee8da0b041812832762).
- Locked dependency: tiktoken-rs 0.12.0. Both prototypes copy the unchanged locked dependency into separate disposable directories, preserve public APIs and keep every dependency version fixed. External Cargo patch overrides remove only that dependency's registry source/checksum from the experimental lockfile.
- All-table native candidate SHA-256 `480bddd1ef8a6bd2a92ec30add633ad8d56d590886137d4541abe9d8705dc820`; dependency patch SHA-256 `295a8b7b4d3e12022fc5c1a2c716a96bd43e34eef63373582b72e30f9ece07da`. Ordinary offline/locked release build, without cfg(test), takes 376 seconds. This is experimental source, not a published product release.
- Schema-v5 profiler source [`b23728ca8e00c99bd968ef0840f6506921555aa1`](https://github.com/morluto/leantoken/commit/b23728ca8e00c99bd968ef0840f6506921555aa1), SHA-256 `8af743da46dd8843f1d6269046d63c4bf5c46d450df1656c75de0e752e290bbf`.
- Isolated Linux x86_64 container: one CPU quota, three GiB, one build/index worker, separate HOME/cache, no network. Heavy builds and probes run serially. The host MCP installation and configuration are unchanged.
- [Machine-readable evidence](tokenizer-ownership-linux-x86_64-2026-10-06.json) contains the locked dependency identity, both exact patches, allocator snapshots, golden controls, complete parsed native matrices, all evaluation cells, binary/raw hashes and reproduction source. Original raw files and failed compiler attempts remain preserved locally.

## Ownership and constructor screen

The earlier [empty-root activation control](shared-cache-mcp-memory-linux-x86_64-2026-10-06.md) found approximately 24 MiB PSS/private growth on exact BPE activation. It did not identify all allocations or prove a leak. Locked dependency inspection finds three retained token-byte collections: encoder, decoder and sorted lookup table. LeanToken's truncation also needs decoder bytes, so removing the decoder is not justified by count-only observations.

An out-of-product GlobalAlloc probe delegates unchanged to System. It measures live requested Rust bytes, cumulative requested-byte peak and allocation/reallocation calls. It excludes allocator headers/slack, C allocations and resident pages. Four fresh processes per implementation construct cl100k_base, repeat four fixed strings 100 times, drop the direct tokenizer and initialize/check the process singleton. No source/index payloads are involved.

| Implementation | Constructor live requested bytes | Reduction | Allocation screen |
| --- | ---: | ---: | --- |
| Original Vec collections | 13,655,864 | baseline | baseline |
| All-table Arc bytes | 11,412,248 | 16.43% | pass |
| Original encoder; Arc decoder/sorted bytes | 13,104,654 | 4.04% | fail |

The screen requires at least 10% live reduction and peak no more than 1.1× original. All-table peak ratio is 0.8065; decoder/sorted peak ratio is 0.9140. All four observations per implementation agree. Repeated direct work adds zero live requested bytes after warmup; dropping the direct tokenizer returns to its 548-byte baseline. These short constructor controls do not establish or exclude a long-lived leak.

The all-table prototype changes encoder key layout to Arc<[u8]> and shares bytes with decoder and sorted handles. The smaller prototype keeps HashMap<Vec<u8>, Rank>, byte-pair helpers and ordinary encoding code unchanged, and shares decoder bytes with sorted handles. Its resident-memory and CPU effects are unmeasured because it fails the allocation screen. Neither screen assigns an individual structure's retained bytes from aggregate process metrics.

## Semantic controls

Each prototype passes 1,209,000 rank checks: ranks 0 through 201,499 for each of r50k_base, p50k_base, p50k_edit, cl100k_base, o200k_base and o200k_harmony. Checks cover valid bytes and invalid-rank errors, valid UTF-8 token-byte re-encoding, 213 fixed/seeded corpus cases per encoding, allowed/disallowed special tokens, owned decoded outputs, 32 unstable-encoding cases and 32 clone-after-original-drop cases per encoding. The all-table fork also passes 14 original dependency unit tests with none ignored.

These are compatibility controls for the measured forks. They do not validate a future backend or make a temporary Cargo patch publication-ready. LeanToken publicly exposes the upstream tokenizer enum, and both count and UTF-8 truncation contracts must remain exact. Cargo and native/npm publication must carry the same supported implementation; the external overrides here are diagnostic only.

## All-table native paired screen

The recipe and criteria are declared before candidate observations. Four complete matrices run in original/candidate/candidate/original order. Each uses 200 Rust files × 40 functions, 1/4/8 native processes, shared/independent caches in ABBA order, 25 warm rounds, nine-second idle windows and a separate 50,001-directory/31-second polling probe. No compiler or extra native source query overlaps sampling.

Memory requires at least 5% reduction in the median of four shared-eight cohort aggregate PSS/private snapshots per binary. CPU/query uses request-weighted totals for each workload across identical topology/count arms; each baseline must reach 200 CPU-ms (20 clock ticks). Each of 24 workload/topology/count latency cells uses the median of its four p95 observations per binary. CPU, p95 and cold CPU/repository ratios must be at most 1.05. Original absolute profiler thresholds remain unchanged.

| Metric | Original | All-table candidate | Change | Gate |
| --- | ---: | ---: | ---: | --- |
| Shared-eight median PSS (KiB) | 250,517 | 187,888 | −25.00% | pass |
| Shared-eight median private resident (KiB) | 229,820 | 167,218 | −27.24% | pass |
| Files CPU, 2,600 requests (ms) | 8,100 | 8,750 | +8.02% | fail |
| Search CPU, 2,600 requests (ms) | 11,780 | 12,430 | +5.52% | fail |
| Read CPU, 2,600 requests (ms) | 15,890 | 17,200 | +8.24% | fail |
| Context CPU, 2,600 requests (ms) | 32,350 | 34,560 | +6.83% | fail |
| Cold CPU, 64 repositories (ms) | 42,110 | 44,210 | +4.99% | pass |

All four workload CPU gates have adequate clock-tick resolution and fail. Six of 24 p95 cells fail:

| Cache topology | Processes | Workload | Candidate p95 change |
| --- | ---: | --- | ---: |
| independent | 4 | context | +7.35% |
| shared | 1 | read | +12.56% |
| shared | 1 | search | +14.95% |
| shared | 4 | context | +6.44% |
| shared | 4 | search | +13.82% |
| shared | 8 | read | +7.29% |

All 21,616 parity checks and 832 baseline fingerprint comparisons pass, including complete owning-session pagination. There are 4,160 page-validation calls, 16 deliberate takeover leader kills and 196 checked successful normal EOF paths across 208 matrix children plus four polling children. Ownership/takeover checks and all 144 complete memory aggregates pass. Concurrent shared-cache accounting remains best-effort/lossy; normalized parity is not an independent complete-wire BPE oracle.

Every raw matrix keeps its absolute `investigate_host_wide_admission` recommendation. Warm p95 ratios are 20.7829, 12.3922, 18.0141 and 19.3649, all above 3.0. Passing compatibility and lower memory do not excuse the CPU/latency failures. No unchanged experiment is rerun to select more favorable measurements.

## Limits and reproduction

PSS apportions shared pages; private resident pages include more than heap. Samples within a cohort are sequential, not atomic. CPU quota is `cpu.max=100000 100000`, while the effective cpuset is 0-15. Linux [CPU bandwidth documentation](https://docs.kernel.org/scheduler/sched-bwc.html) explains quota exhaustion can throttle threads until replenishment. The captured cumulative cgroup counters have no pre-arm baseline, so they do not attribute a particular latency cell or the older host CPU symptom. Quota/affinity/criteria were unchanged throughout sampling.

Extract the two source patches and probe sources from the evidence JSON. Apply each patch to a fresh copy of the locked dependency; retain the original dependency for the old_token reference in the golden probe. The archived compile arguments identify the exact original dependency artifacts used by the component probes. For the all-table native experiment, check out candidate base 988bb0830, override only the copied dependency with Cargo's patch.crates-io configuration, verify all other lockfile packages are unchanged, and build offline/locked release with one job. Build the original CLI and v5 profiler from their separately linked revisions and seal all binaries before sampling.

Run four full matrices in the fixed order, substituting the selected sealed binary:

```bash
mcp_multiprocess_profile --binary /path/to/sealed/leantoken \
  --max-index-workers 1 --process-counts 1,4,8 \
  --files 200 --functions-per-file 40 --warm-iterations 25 \
  --idle-seconds 9 --polling-directories 50001 \
  --polling-observation-seconds 31 --timeout-seconds 30 \
  --output unique-arm.json
```

The evidence includes the actual native driver and evaluator, their original paths, manifests and raw SHA-256 values. Run component probes first and stop when their predeclared floor fails. #604 remains open: allocation-owner attribution is stronger, but an eligible publication-compatible reduction still needs its own measured correctness/resource gates. This report does not claim a production memory fix or a diagnosed days-long leak.
