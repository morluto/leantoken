# Streaming index publication profile

Date: 2026-07-20

Host:

- Linux 6.1.0-50-cloud-amd64 x86_64
- rustc 1.97.0 (2d8144b78 2026-07-07)
- cargo 1.97.0 (c980f4866 2026-06-30)
- release profile

## Comparison

The baseline is detached revision `e38693e` (`fix: share repository discovery
policy`). The candidate is the streaming-publication change containing this
report. Both runs used independently built release binaries and the same
command arguments:

```bash
indexing_profile \
  --files 7500 \
  --file-bytes 8192 \
  --iterations 1 \
  --read-samples 1 \
  --hot-set 1
```

A shell sampler read `/proc/<leantoken-pid>/status` every 50 ms and retained
the largest `VmRSS`. This is sampled high-water evidence, not allocator-level
accounting, but the same sampler and host were used for both revisions.

| Revision | Admitted files | Source bytes | Initial index | Sampled peak RSS |
| --- | ---: | ---: | ---: | ---: |
| `e38693e` baseline | 7,501 | 61,447,529 | 31.78 s | 143,728 KiB |
| streaming candidate | 7,501 | 61,447,529 | 24.21 s | 82,092 KiB |

The candidate reduced sampled peak RSS by 42.88% and initial-index wall time by
23.82% on this run. Its schema-v5 phase diagnostics reported 30 preparation
batches, a 256-file high-water, and a 2,097,408-byte discovered-source
high-water per batch.

## Boundary corpora

One synthetic run used 34 files of 2,000,000 bytes each, plus the profiler's
ignore control file. It admitted 35 files / 68,000,063 bytes across two batches.
The largest batch held 34 files / 66,000,062 discovered source bytes, below the
configured 64 MiB bound, and sampled peak RSS was 153,272 KiB.

One real-repository run used a disposable snapshot of Tokio revision
`9cae638de6dc8dd9779c450201df8c102247a242`. It admitted 865 files / 5,989,997
bytes across four batches, with a 256-file / 2,429,750-byte batch high-water and
82,620 KiB sampled peak RSS.

## Interpretation limits

- The synthetic source shape is one Rust item followed by deterministic comment
  padding; it does not represent every parser-output distribution.
- The 7,500-file run covers both high file count and 8 KiB source. The large-file
  run exercises the byte boundary, while Tokio supplies a real directory and
  language mix. It is not a cross-platform or all-monorepo conclusion.
- RSS includes the runtime, parser libraries, SQLite, and allocator behavior.
  The batch diagnostics measure discovered source bytes, not all derived rows.
- Repeat the pinned corpus and sampler on macOS and Windows before treating the
  absolute numbers as portable.
