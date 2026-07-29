# Explicit index-scope mechanism profile

Date: 2026-07-29

Decision: ship explicit, cache-identified indexing scope as an opt-in
correctness boundary. Keep full-repository indexing as the default.

## Evidence identity

- Corpus: `https://github.com/tile-ai/TileLang.git` at
  `eb31994ad782108d8754b19603b428eca9c1e19d`, including its recorded recursive
  submodules.
- LeanToken source revision: `adf989a8af5e`.
- Release binary SHA-256:
  `4646f293979107ef123c3babea46a1ae6429baaaa1d308e962a1af71cae85716`.
- Host: Linux `6.1.0-50-cloud-amd64`, x86-64, eight available processors.
- Toolchain: `rustc 1.95.0 (59807616e 2026-04-14)`.
- Both arms used two index workers, a fresh explicit SQLite path, the same
  checkout, and the release binary. The scoped arm added only
  `--index-exclude '3rdparty/**'`.

The commands were:

```bash
/usr/bin/time -f 'wall_s=%e user_s=%U sys_s=%S max_rss_kb=%M' \
  target/release/leantoken \
  --root target/profile-repos/TileLang \
  --database /tmp/.../full.sqlite \
  --max-index-workers 2 --json index

/usr/bin/time -f 'wall_s=%e user_s=%U sys_s=%S max_rss_kb=%M' \
  target/release/leantoken \
  --root target/profile-repos/TileLang \
  --database /tmp/.../scoped.sqlite \
  --max-index-workers 2 \
  --index-exclude '3rdparty/**' --json index
```

## Results

`index_storage_bytes` is the immediate post-index sum reported by `status` for
the SQLite main, WAL, and SHM files.

| Metric | Full | Scoped | Reduction |
| --- | ---: | ---: | ---: |
| Files seen | 34,322 | 1,383 | 95.97% |
| Files indexed | 33,928 | 1,351 | 96.02% |
| Indexed source bytes | 484,403,939 | 13,714,653 | 97.17% |
| Chunks | 126,011 | 5,213 | 95.86% |
| Symbols | 1,392,610 | 18,771 | 98.65% |
| Wall time | 390.51 s | 9.04 s | 97.69% |
| Process CPU, user + system | 506.08 s | 12.99 s | 97.43% |
| Maximum RSS | 145,724 KiB | 68,044 KiB | 53.31% |
| Immediate index storage | 4,861,155,528 B | 144,761,856 B | 97.02% |

The first scoped arm indexed 1,351 files in 9.04 seconds. A second fresh-cache
scoped arm indexed the same 1,351 files with the same skip counts in 9.16
seconds, using 13.01 CPU seconds and 67,480 KiB maximum RSS. This repeat checks
that the small-arm result was not a one-off process anomaly.

The 43.20× wall ratio is not the product contract. The stable mechanism claim
is narrower: rejecting `3rdparty/**` before descent removed 96.02% of indexed
files and therefore avoided their preparation, parsing, FTS publication, and
storage. This profile does not claim that the default full-repository index
became faster.

## Retrieval parity and provenance

An exact 100-token read of `tilelang/__init__.py` against both committed
generation-one caches returned byte-identical source and coordinates:

- `content_hash`: `40f43e041873fb7a0b39f8c4bd874218`;
- `indexed_hash`: `d80f34de9cf9ada0cba160fc1c235647`;
- continuation cursor:
  `1:read:1:234:17:485:d80f34de9cf9ada0cba160fc1c235647:a2ce99dcdd0cccc2`.

The full response reported `meta.index_scope: "full"`. The scoped response
reported `meta.index_scope: "scoped"` and digest
`8d1eb8e4ae57f138`. The scoped response's additional provenance increased its
complete response accounting, but did not change the selected source.

Behavioral integration tests additionally cover empty excluded-dependency
searches, scope-normalized cache separation, explicit-database rejection,
targeted additions/deletions and cross-boundary renames, ignore-control
changes, watcher fallback, and full-versus-scoped selected-hit parity.

## Limitations

- This is one pinned dependency-heavy corpus on one Linux host.
- The operating-system page cache was not evicted. Full ran before scoped, so
  wall time is paired mechanism evidence, not a cold-disk or cross-host
  distribution.
- Full has one sample in this run because it costs roughly 6.5 minutes. The
  earlier pinned TileLang cold-index matrix contains two independent full
  two-worker samples at 337.06 and 416.05 seconds; it used an older LeanToken
  revision and is corroborating context, not part of this percentage
  calculation.
- Maximum RSS comes from GNU `time`; storage is an immediate post-index value,
  not a sampled peak. No claim is made about short-lived WAL peaks.
- Excluded dependency results are intentionally unavailable. Empty scoped
  retrievals disclose their boundary and must not be interpreted as
  whole-repository absence.
