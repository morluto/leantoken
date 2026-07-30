# LeanToken Developer Experience Audit — July 2026

**Auditor:** Linzumi Codex  
**Scope:** Full codebase (~93K LOC, ~60K production LOC)  
**Method:** Static analysis, test execution, architectural review, performance profiling  
**Date:** 2026-07-30

---

## Executive Summary

| Category | Finding | Severity |
|----------|---------|----------|
| **Critical Bug (Fixed)** | Process tests fail when parent shell has `npm_lifecycle_event=npx` | 🔴 High |
| **Performance** | Blocking executor hard-coded to 8 threads | 🟡 Medium |
| **Performance** | Regex full-scan fallback can scan 1M chunks (~4GB) | 🟡 Medium |
| **Performance** | `json_tokens` serializes to string on every check | 🟡 Medium |
| **Reliability** | 319 `expect`/`unwrap`/`panic` in production code | 🟡 Medium |
| **Maintainability** | 8 files >800 LOC, largest 1,720 LOC | 🟡 Medium |
| **Performance** | SQLite reader pool fixed at 8 connections | 🟡 Medium |
| **Performance** | Git subprocess overhead in history (2 per batch) | 🟡 Medium |
| **Test Speed** | Integration tests: 53s single-threaded, no shared fixtures | 🟡 Medium |
| **Code Quality** | 22 clippy `allow` overrides mask real issues | 🟢 Low |

**Positives:**
- ✅ Zero `unsafe` blocks (enforced by lint)
- ✅ No TODO/FIXME/HACK in production code
- ✅ Clean workspace DAG, no circular dependencies
- ✅ No blocking I/O in async functions — all heavy work goes through `spawn_blocking`
- ✅ SQLite WAL + snapshot isolation is correct
- ✅ Deterministic ranking and bounded memory are preserved

---

## 1. Critical Bugs

### 1.1 Process Tests Fail Due to Inherited `npm_lifecycle_event` 🔴

**Status:** **FIXED** in PR #395 (`fix/process-test-env-isolation`)

**Affected tests:**
- `setup_and_remove_do_not_require_a_repository` (line 1624)
- `setup_dry_run_reports_exact_plan_without_mutation` (line 1938)

**Root cause:** `McpLauncher::current()` at `src/setup/launcher.rs:18` checks `std::env::var_os("npm_lifecycle_event")`. When the parent shell has this set (common in npx-based dev environments), the test binary detects npx mode and:
1. Tries to run `npx --yes leantoken@0.1.20 doctor --json` → fails (package not published)
2. Returns `package: "leantoken@0.1.20"` instead of `null` in dry-run output

**Fix:** Added `.env_remove("npm_lifecycle_event")` to all non-npx process tests in `tests/process.rs`.

**Validation:** `cargo test --test integration -- --test-threads=1` → 270 passed, 0 failed.

---

## 2. Performance Bottlenecks

### 2.1 Blocking Executor Hard-Coded to 8 Threads

**Location:** `src/services/executor.rs`

**Architecture:** Two-tier semaphore — `active` (16) admits requests, `execution` (8) runs on `spawn_blocking`. Queue timeout: 500ms.

**Impact:**
- On 16+ core machines, 50% of CPU sits idle during indexing/search storms
- No lane separation — indexing can starve read/search requests
- Concurrency profile notes `split_execution_lanes: false`

**Recommendation:**
1. Make execution threads configurable (`LEANTOKEN_EXECUTION_THREADS` or `config.toml`)
2. Default to `num_cpus::get()` or `num_cpus::get() * 2`
3. Consider lane separation: indexing lane vs retrieval lane

### 2.2 Regex Full-Scan Fallback Scans Up to 1M Chunks

**Location:** `src/services/search/execution.rs`

**Math:** `MAX_REGEX_FILES_SCANNED` (10k) × `MAX_REGEX_CHUNKS_PER_FILE` (100) = 1M chunks. At ~4KB/chunk = **4GB scanned per request**.

**Impact:** A single regex search can consume all 8 blocking threads for seconds. No production monitoring of fallback rate.

**Recommendation:**
1. Add metrics/logging for regex fallback rate
2. Reduce caps under load
3. Improve trigram extraction in `regex_plan.rs`
4. Consider early-abort on time budget exceeded

### 2.3 `json_tokens` Serializes Value to String on Every Check

**Location:** `src/services/json.rs`

**Pattern:** `json_tokens()` does `serde_json::to_string(value)` then counts tokens. In `project_schema_page`, this happens inside a **binary search loop**.

**Impact:** O(log n × tree_size) serialization work per request. Large JSON files cause repeated allocations on blocking threads.

**Recommendation:** Cache token counts per schema node, or use a streaming tokenizer.

### 2.4 SQLite Reader Pool Fixed at 8 Connections

**Location:** `src/storage/`

**Impact:** Under 32 parallel search requests (concurrency profile), readers wait for pool checkout. First 8 concurrent requests pay lazy connection establishment cost.

**Recommendation:** Make pool size configurable, add connection warmup, monitor checkout waits.

### 2.5 Git Subprocess Overhead in History Service

**Location:** `src/services/history.rs`

**Impact:** Every `DiffSymbols` call spawns `git ls-tree` + `git cat-file --batch`. No blob caching — same revision+path parsed fresh every request. `fit_diff_symbols_response` clones entire response during binary search.

**Recommendation:** Cache blobs in LRU, batch all targets in one `cat-file --batch`, use reference-counted strings.

---

## 3. Reliability Issues

### 3.1 319 Crash Points in Production Code

**Count:** 299 `expect()`, 11 `unwrap()`, 9 `panic!`

**Worst offenders:**
| File | Count |
|------|-------|
| `src/coordination.rs` | 31 |
| `src/episode.rs` | 30 |
| `src/services/executor.rs` | 29 |
| `src/services/concurrency_profile.rs` | 23 |
| `src/services/startup/mod.rs` | 22 |
| `src/services/read_delta.rs` | 22 |
| `src/services/json.rs` | 20 |

**Impact:** Any unexpected state causes hard crash instead of graceful error. `coordination.rs` and `episode.rs` are core orchestration — a single bad input crashes the whole service.

**Recommendation:** Replace `expect()` in core files with `map_err` + structured errors. Audit the 11 `unwrap()` calls. Consider a lint limiting `expect()` to initialization code.

---

## 4. Maintainability Issues

### 4.1 Large Files Need Decomposition

| File | Lines | Functions |
|------|-------|-----------|
| `src/services/json.rs` | 1,720 | 50 |
| `src/services/history.rs` | 1,417 | 35 |
| `src/services/context/facets.rs` | 1,127 | — |
| `src/model/context.rs` | 1,029 | — |
| `src/doctor.rs` | 907 | 30 |
| `src/services/concurrency_profile.rs` | 904 | 30 |
| `src/services/files.rs` | 891 | 34 |
| `src/config.rs` | 861 | — |
| `src/services/read_delta.rs` | 825 | 21 |

**Recommendation:** Split `json.rs` into `json/{query,projection,schema,cursor}.rs`. Split `history.rs` into `history/{diff,symbol,git,fitting}.rs`. Add CI warning for files >800 LOC.

### 4.2 22 Clippy `allow` Overrides

**Breakdown:** 14 `too_many_arguments`, 6 `cast_precision_loss`, 2 `cognitive_complexity`

**Impact:** `too_many_arguments` suggests functions doing too much. `cognitive_complexity` marks the most complex functions as "allowed" instead of refactored. `cast_precision_loss` may hide real precision bugs.

**Recommendation:** Remove overrides and fix root causes. Add CI gate preventing new `allow` without justification.

---

## 5. Test Suite Analysis

### 5.1 Timing

| Suite | Tests | Time (single-threaded) | Time (default threads) |
|-------|-------|------------------------|------------------------|
| Unit (`--lib --bins`) | ~504 | ~8.3s | ~8.3s |
| Integration | 270 | 53.34s | ~7-15s |
| Process tests | 42 | ~7.3s (2 threads) | ~7.3s |
| Test-suite crate | 133 | ~2.2s | ~2.2s |

### 5.2 Structural Issues

- **No shared fixtures:** Every test creates fresh `tempfile::tempdir()` — 150 usages
- **Process tests spawn real binary:** ~1s each via `Command::cargo_bin("leantoken")`
- **No `lazy_static`/`once_cell` for immutable fixtures:** Pre-built git repos could be shared
- **74 Duration/timeout/sleep references** in process tests alone

**Recommendation:**
1. Profile per-test timing to find 10 slowest
2. Share immutable fixtures (pre-built git repos, compiled test binaries)
3. Separate process tests into own test target for parallel CI
4. Evaluate `cargo nextest` for better parallelism

---

## 6. Architecture Strengths

| Strength | Evidence |
|----------|----------|
| **Zero unsafe** | `unsafe_code = "forbid"` lint enforced |
| **No blocking I/O in async** | All file reads, SQLite, git, regex in `spawn_blocking` |
| **Clean workspace DAG** | No circular dependencies |
| **SQLite WAL + snapshot isolation** | `DEFERRED` transactions, atomic generations |
| **Bounded behavior** | Token budgets, memory limits, timeout caps throughout |
| **Deterministic ranking** | Documented and tested |
| **No TODO/FIXME in production** | `todo = "warn"` lint enforced |

---

## 7. Prioritized Remediation Roadmap

### P0 — Critical (Fix Immediately)
- [x] **Process test env isolation** — PR #395 (merged)

### P1 — High Impact (Next Sprint)
- [ ] **Make blocking executor threads configurable** — `src/services/executor.rs`
- [ ] **Add regex fallback metrics + reduce caps under load** — `src/services/search/execution.rs`
- [ ] **Reduce `expect()` density in `coordination.rs` and `episode.rs`** — core reliability
- [ ] **Decompose `json.rs` and `history.rs`** — maintainability

### P2 — Medium Impact (Backlog)
- [ ] **Cache token counts in JSON schema projection** — `src/services/json.rs`
- [ ] **Make SQLite reader pool size configurable** — `src/storage/`
- [ ] **Cache git blobs in history service** — `src/services/history.rs`
- [ ] **Remove clippy `allow` overrides** — code quality
- [ ] **Share test fixtures + evaluate nextest** — test speed

---

## 8. Metrics Dashboard

| Metric | Value |
|--------|-------|
| Production LOC | ~59,787 |
| Total `.rs` files in `src/` | 251 |
| Async functions | 208 (~21% of 983 total) |
| `tokio::spawn` calls | 39 |
| `expect()` in production | 299 |
| `unwrap()` in production | 11 |
| `panic!` in production | 9 |
| `unsafe` blocks | 0 |
| TODO/FIXME/HACK | 0 |
| Clippy `allow` overrides | 22 |
| Files >800 LOC | 8 |
| Max module depth | 4 |
| Workspace crates | 5 |
| Circular dependencies | 0 |
| Integration tests | 270 |
| Unit tests | ~504 |
| Process tests | 42 |
| `tempfile::tempdir()` usages | ~150 |

---

*End of audit report.*
