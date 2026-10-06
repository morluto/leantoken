# Exact source/chunk token-count reuse

Date: 2026-10-06. Follow-up to #612.

Decision: reject the disposable prototype. Neither representative corpus meets
the preregistered 5% reduction in median initial-index CPU. The production
preparation code is restored; this report retains the negative result.

## Evidence identity

- LeanToken 0.1.28, Rust 1.95.0, Linux 6.1.0-53-cloud-amd64, x86_64.
- Disposable network-disabled container: one CPU, 3 GiB memory, no extra swap,
  one Cargo job and one preparation worker. Builds and measurements run serially.
  The host MCP installation and configuration remain unchanged.
- Baseline executable comes from `0bf353926e76a349d2eedae38cc3ee2e2130ca0d`.
  Its complete production `src`, Cargo manifests/lockfile and build script have
  zero diff against experiment base `c083b2883af9c6188c9085d61ca8c776ca8472fc`.
- Candidate is that experiment base plus the uncommitted patch recorded in the
  [raw report](exact-token-count-reuse-linux-x86_64-2026-10-06.json). The report
  binds both executable SHA-256 values, patch, corpus trees/file-manifest hashes,
  locked package checksums, all 18 samples and their parity fingerprints.
- Raw report: 46,727 bytes, SHA-256
  `70ed37f1af7bd4f59d596149d9c964d1f679cccc8c8174b830bd44b69df900b9`.

## Hypothesis and frozen policy

`src/indexer/prepare.rs` counts the complete source and each prepared chunk
using the same tokenizer. Reuse the complete-source count only when the chunk
bytes equal the entire prepared source. Other chunks keep their existing count.
This creates no cache or allocation and changes no parsing or chunk boundaries.
BPE counts across distinct chunks are not additive.

Before the candidate, require at least 5% lower median initial-index total CPU
on a representative corpus, at most 2% median CPU regression on either holdout,
no unexplained peak-memory increase above 5%, and exact index/retrieval parity.
Run each corpus in `A B B A A B` order with three observations per version.
Each arm has a fresh process, SQLite database, HOME and cache. Cold means
generation zero; the operating-system page cache is not evicted.

## Frozen corpora and duplicate work

| Corpus | Tracked files / bytes | Indexed files | Exact whole-source chunks | Repeated tokens / all counted tokens |
| --- | ---: | ---: | ---: | ---: |
| LeanToken at `c083b2883` | 625 / 14,083,905 | 620 | 158 | 51,613 / 5,600,560 (0.9216%) |
| Tokio 1.52.3, serde_json 1.0.150, syn 2.0.119 | 749 / 7,677,381 | 748 | 301 | 79,893 / 3,869,983 (2.0644%) |
| Synthetic multichunk Rust | 2,000 / 16,433,500 | 2,000 | 0 | 0 / 11,928,180 (0%) |

The dependency fixture copies locked package source, excluding registry
installation metadata `.cargo-ok` and `.cargo-checksum.json`, then tracks all
files in a fresh Git repository. The synthetic fixture creates `file_0000.rs`
through `file_1999.rs`. For file index `i`, append the following UTF-8 line for
successive zero-based `j` until the file contains at least 8,192 bytes:

```text
pub fn synthetic_{i}_{j}() -> usize { {i} + {j} }
```

Each line ends with a newline. This produces 6,000 chunks and no byte-equal
whole-source chunk. Eligibility is proved by comparing each stored chunk's
UTF-8 bytes with the original file bytes and checking the exact token counts.
These token fractions characterize work; they are not CPU savings estimates.

## Initial-index results

| Corpus | Baseline median CPU | Candidate median CPU | CPU change | Baseline / candidate median wall | Baseline / candidate median peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| LeanToken | 8.476767 s | 8.506395 s | +0.35% | 8.630080 / 8.680056 s | 73,428 / 73,736 KiB |
| Locked dependencies | 7.152152 s | 6.922422 s | -3.21% | 7.376871 / 7.025116 s | 72,352 / 72,036 KiB |
| Synthetic multichunk | 18.857840 s | 17.978952 s | -4.66% | 19.172046 / 18.264944 s | 69,360 / 69,360 KiB |

The supervisor measures only the initial CLI operation with `wait4`: user plus
system CPU, per-process `ru_maxrss`, and monotonic elapsed time with a 50 ms
polling resolution. Post-index parity requests are excluded. Each initial index
has a 120-second deadline; each retrieval has a 30-second deadline.

No corpus reaches the 5% CPU screen. The zero-opportunity control also changes
by 4.66%, so small differences cannot be assigned to token-count reuse. Three
samples per version on a shared host do not establish statistical significance.
Worker-duration attribution from the earlier issue is neither isolated phase
CPU nor additive wall time; this experiment measures total initial-process CPU.

## Correctness and reproduction

All 18 arms exit zero and publish generation one. Complete semantic fingerprints
match for files, chunks, symbols, references, imports and import candidates,
including source/chunk token counts and tokenizer identity. Surrogate IDs are
excluded; import candidates are compared through their source file/import
identity. All four searches, one bounded exact read and one bounded context
match across all six arms of each corpus.

The frozen search queries are `pub fn`, `use`, `token_budget`, and
`CancellationToken`, in text mode, with eight results, zero context lines and
1,000 source tokens. Read the first indexed path in lexical order, lines 1:80,
with 500 source tokens. Context uses task
`Investigate token_budget and CancellationToken implementation`, 1,000 source
tokens and eight fragments. All requests use `indexed_generation` consistency
and a 10,000-token response envelope. Comparisons remove only generated receipt
and repository identity, freshness, and path/envelope token totals from `meta`;
source, order, coverage, selection and source/protocol accounting remain exact.

Build each release executable with `cargo build --locked --release --package
leantoken --bin leantoken`. For each fresh arm, run:

```bash
leantoken --root "$corpus" --database "$database" \
  --max-index-workers 1 --tokenizer cl100k_base --json index
```

The disposable candidate also passes all 55 focused all-feature indexer tests.
This is an initial-index operation study, rather than a worker-concurrency
promotion matrix; it does not replace the guarded cold matrix's cancellation,
restart and phase requirements. Since the improvement screen fails, no product
optimization is promoted. The frozen retrieval checks do not establish broader
task success, provider savings or cross-platform performance.
