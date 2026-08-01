# Root-cleanup regex baseline

This release-mode baseline was captured before the aggregate regex work budget
was implemented. The worktree contained only the independent search-routing
error change, which does not enter regex execution.

- Revision: `e05146c30304ac9c664d6d0d13749063a818f38f`
- Host: Linux x86_64, 16 logical CPUs, 30.2 GiB RAM
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`
- Tokenizer: `cl100k_base`
- Corpus: generated 10,000-file boundary fixture, 731,463 source bytes,
  repository generation 1
- Raw report: `target/root-cleanup-regex-baseline.json`
- Raw report SHA-256:
  `5a6c8c609cb289ee80e1b13b433d3b92971926739a5a0dee506703596653d2e9`

Reproduce with:

```bash
cargo run --locked --release --package leantoken-benchmarks \
  --bin regex_fallback_profile -- \
  --output target/root-cleanup-regex-baseline.json
```

| Workload | Exact parity | Candidate files | Chunks/bytes admitted | Optimized time | Forced full-scan time |
| --- | --- | ---: | ---: | ---: | ---: |
| Sparse positive | yes | 10,000 | 10,255 / 731,463 corpus bytes | 224.27 ms | 168.19 ms |
| Common positive | yes | 10,000 | 10,255 / 731,463 corpus bytes | 229.54 ms | 186.19 ms |
| Boundary negative | yes | 10,000 | 10,255 / 731,463 corpus bytes | 211.95 ms | 171.74 ms |

The unchanged-tree comparison after adding the safety budget also preserved
exact results and the same 10,000-file/10,255-chunk work counts. Optimized times
were 207.65 ms, 229.57 ms, and 221.43 ms respectively. This does **not** support
a representative latency claim: the budget is deliberately inactive below its
calibrated boundary. Its benefit is a finite typed stop for larger pathological
workloads, not faster execution for admitted work.

The three workloads establish a 10,255-chunk observed boundary. The production
chunk budget is twice that value (20,510). The byte budget is twice the largest
representative indexed corpus already retained in the indexing reports,
rounded to 1 GiB and clamped below the 2 GiB index ceiling. The safety budget
does not claim a latency win for legitimate requests; it bounds pathological
fallback work and returns an explicit incomplete result on exhaustion.
