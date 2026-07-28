# Exhaustive regex fallback profile

This profile measures the existing bounded full-scan implementation before any
interval-tree or streaming redesign. The generated corpus reaches both
production boundaries: 10,000 files and 256 chunks in one file. The other
9,999 files contain one chunk each, so every workload verifies 10,255 chunks.

Run from revision `5329a577d6bb12990d766d37492bdf8f0ac3bf7b` with a dirty
working tree containing the profiler and counter instrumentation:

```bash
cargo run --release --example regex_fallback_profile -- \
  --output target/regex-fallback-profile.json
```

Host: Linux x86_64, Rust 1.95.0, 4 logical CPUs, 15.6 GiB RAM. Peak RSS uses a
1 ms `/proc/self/status` sampler combined with `VmHWM`. Each workload runs in a
fresh child process. The table reports the second exhaustive scan in that
process so one-time service initialization is excluded from the RSS delta.

| Workload | Exact parity | Files | Scanned chunks | Retained chunks | Hits | Time | Peak RSS | Search RSS delta |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Sparse positive | yes | 10,000 | 10,255 | 1 | 1 | 304.81 ms | 39,960 KiB | 1,700 KiB |
| Common positive | yes | 10,000 | 10,255 | 100 | 100 | 331.42 ms | 40,104 KiB | 1,724 KiB |
| Boundary negative | yes | 10,000 | 10,255 | 0 | 0 | 430.83 ms | 39,940 KiB | 1,724 KiB |

“Exact parity” compares the complete optimized and forced-full-scan
`SearchResponse` values after removing the opaque receipt identifier and the
three token-accounting fields derived from its serialized value. All three
case-insensitive patterns selected the production `full_scan` strategy, and
the scanned and verified counters were equal.

The audit’s suggested 10,000 × 256 simultaneous-chunk footprint does not match
the implementation: the scan loads and releases one file’s chunk vector at a
time, retaining only matching chunks for occurrence hydration. At both current
bounds, this fixture adds less than 1.7 MiB over the warmed process and finishes
within 431 ms. This evidence does not justify an interval tree or streaming
rewrite.

The result is deliberately narrow. Generated text is not a substitute for a
distribution of real repository chunk sizes, and peak RSS includes the full
search response and SQLite/service state. The profiler remains available for
larger-content real-repository follow-up if production traces show this fallback
to be material.
