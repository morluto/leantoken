# Roadmap

LeanToken's roadmap is evidence-driven. A feature should reduce wasted model
reads or improve relevant-range recall before it expands the MCP tool surface.

## Retrieval quality

- Keep the eight future-fix tasks and the four prospective open-issue tasks as
  visible development sets. Create a new unseen holdout before making
  generalization claims; once used for tuning, a dataset is no longer blind.
- Continue improving useful-line recall without trading away file recall.
  Adaptive ranges preserve exact internal matches and prefer complete
  declarations when they fit. Concept allocation and qualified-owner matching
  must earn their place through frozen ablations.
- The frozen prospective-validation ablation for `2c0388d` preserves early task
  nouns instead of preferring later words only because they are longer. File
  recall increased from 7/11 to 8/11 and labeled-line recall from 13/38 to
  17/38; the Express task increased from 1/3 files and 0/11 lines to 2/3 files
  and 4/11 lines. Dead-end source fell by 58 tokens while complete first-response
  JSON increased by 228 tokens and complete two-turn JSON by 43 tokens. The
  consumed blind holdout was not rerun or used for this tuning.
- Expand the evaluation across more languages and task shapes before making
  broad retrieval claims. Record dead-end source, repeated ranges, known-hash
  resends, and complete two-turn cost alongside recall.
- Candidate-stage diagnostics now distinguish generation from selection without
  expanding runtime responses. On the prospective validation set, candidate
  file recall was 11/11 while returned recall was 8/11. A Tree-sitter signature
  boundary correction improved returned recall to 9/11 and labeled-line recall
  from 17/38 to 21/38 while reducing dead-end source by 140 tokens. A path-score
  candidate reached 10/11 on validation but regressed the consumed holdout and
  was removed. Collect a new unseen holdout before treating the retained result
  as general.
- Add a language grammar only when a pinned task and parser fixture demonstrate
  recall value that outweighs its binary, indexing, and schema cost. The
  expanded task set uses existing grammars, so no grammar was added.
- Structural JSON retrieval now handles exact ignored artifacts through
  Pointer/JMESPath selection, collapsed/key/schema projections, numeric
  summaries, and selected-field diffs without indexing raw reports.
- Context can now return a bounded metadata-only query plan before source
  materialization. Plans reuse hard scopes and ranking, expose scores, reasons,
  exact token estimates, focus coverage, and generated-artifact warnings, and
  do not create or update receipts.

## Token accounting

- Exact local modes now cover the bundled `tiktoken-rs` encodings; an explicit
  inexact estimate mode covers providers without a local vocabulary.
- MCP accounting includes initialization, the eight-tool catalog,
  `notifications/initialized`, JSON-RPC envelopes, results, and handoffs. A
  transparent stdio proxy can capture exact exchanges from real hosts.
- Compare dual, text-only, and structured-only results per host/version. Keep
  dual as the default until a captured compatibility matrix proves a smaller
  mode reaches the model correctly.
- Codex CLI 0.144.5 has one captured dual-mode exchange covering initialization,
  catalog listing, and two tool calls. It confirms dual delivery for that exact
  host/version but does not justify changing the default for other hosts.
- Codex CLI 0.144.1 now has a redacted host-rollout/MCP receipt covering
  initialization, catalog listing, three tool calls, a known-hash
  `not_modified` follow-up, provider-native cumulative usage, and two
  compactions. The matching local wire contains 4,483 tokens and 776 tokens of
  exact dual-result duplication, but no provider request frame was exported.
  Treat Phase 3A as measured but provider-framing-inconclusive; do not start
  Phase 3B or claim provider savings from the local duplication count.
- A separate Codex CLI 0.144.1 root-plus-child pilot consumed structured-only
  results successfully. On its visible owner-tracing task, dual results copied
  34,656 text bytes beside 34,564 structured bytes; structured mode removed the
  text copy. A general lexical-owner candidate then recovered all four exact
  path/symbol labels. This proves structured consumption for that frozen host
  path, not compatibility for other clients or a provider-cost win; keep dual
  as the global default until the compatibility matrix is broader.
- The checked 2026-07-20 compatibility matrix now binds both Codex captures to
  their artifact identities and records every requested host across dual,
  text-only, and structured-only modes. Claude Code, Cursor, Gemini CLI, and
  OpenCode were unavailable in the audit environment, so their fields remain
  null and unknown. The matrix therefore retains `dual` globally; extend it
  only with sanitized real-host evidence from an available exact version.
- The frozen 2026-07-21 MCP response ablation accepts one representation-
  neutral change: omit the internal task fingerprint from serialized context
  receipts. It reduces the fixed response JSON by 18 exact local tokens and
  the complete dual wire by 39 without adding exact resends or overlapping
  source. Freshness, ranges, omission details, readable reasons, and aligned
  hashes remain; structured-only stays scoped to Codex CLI 0.144.1 and
  provider-input savings remain unknown.
- Two 60-run Codex CLI 0.144.1 suites now cover four pinned Python, Go,
  JavaScript, and Rust validation tasks. Full-history to context-free native
  forks saved 21.6% and 21.9% total input in the two runs. An iterative
  structured LeanToken profile was negative, using 50.9% more input than thin
  native because its child averaged 8.2 provider requests. A frozen
  one-context-plus-optional-one-search profile instead saved 20.1% versus fresh
  thin native and 37.6% versus full native, with bootstrap lower bounds of
  13.4% and 32.3%, 15/20 path-set successes, and no contract violation. Treat
  this as evidence for a bounded triage subagent profile, not permission to
  restrict implementation agents or change the cross-host `dual` default.
- Representation tests compare context fragments, search excerpts, outlines,
  full reads, and a compact repository tree under visible source and complete
  JSON token counts.
- Add model input framing and provider-native counts where hosts expose them.
  Never silently substitute a local tokenizer for provider billing counts.

## Runtime footprint

- MCP processes already share one repository cache, cross-process
  reconciliation lock, indexing leader, and watcher; followers take over after
  leader failure. Private-runtime setup registers the versioned native binary
  directly instead of retaining npm and Node wrappers.
- Status now exposes current-process RSS, SQLite main/WAL/SHM bytes, indexed
  source bytes, and index amplification. Cross-process follower counts and
  aggregate RSS remain future work because file-lock ownership alone cannot
  identify every live client accurately.

## Model behavior

- Run the seeded isolated A/B harness on repeated tasks: filesystem, frozen
  baseline LeanToken, frozen adaptive LeanToken, and adaptive discovery with
  native recovery. Keep prewalk handoff as an optional additional arm.
- Improve tool descriptions and examples only when traces show fewer broad,
  repeated, or dead-end reads. Do not add a runtime “next action” field merely
  because it sounds helpful.
- Keep LeanToken responsible for transferring grounded evidence, receipts, and
  repository generations—not for model sessions or agent execution.

## Optional context signals

- Repository-generation and bounded working-tree changes are optional additive
  boosts. The Git probe has a 500 ms process timeout and normalizes paths for a
  repository root nested below the worktree; failure removes the signal instead
  of failing retrieval. File modification time is not used as a recency proxy
  because fresh checkouts make it misleading.
- Keep the existing bounded import-neighbor signal visible by representation.
  The frozen eight-task development ablation produced no corroborated import
  candidates. Reverse dependency changed no file or line recall and only 2/17
  signal candidate files were relevant. Parsed caller candidates gained one
  line anchor, but only 8/135 candidate files were relevant and complete
  response cost increased by 1,068 tokens. Retain no new reverse-dependency or
  caller boost and expose no graph metadata. Reconsider only with newly frozen
  evidence that beats lexical retrieval on the same recall, dead-end, complete
  response, and precision gates.
- Do not add a hot-file or retrieval-result LRU. Complete live reads now fuse
  hashing and range extraction into one forward stream, while truncated reads
  retain a verification pass. Frozen progressive traces contain only 4 exact
  range rereads across 141 retrieval calls; 57 overlapping rereads do not prove
  identical generation-scoped primitive reuse. A controlled 12-request OpenClaw
  replay did find 1,796 exact reuses among 2,224 generation-scoped primitive
  calls, but it deliberately repeated identical request shapes. Collect
  production-like arrival order, repeat distance, and byte-weighted hit
  potential before prototyping a cross-request primitive cache.
- Keep context hydration batched per query for now. The 2,000-file hot-path
  diagnostic found no duplicate hydration work. On OpenClaw, a realistic
  request made 12 adaptive, 8 enclosing-symbol, and 4 stored-excerpt batches,
  but all 410 requested facts were unique. Diagnostic timings place lexical FTS
  at roughly 0.37–0.46 seconds, while enclosing lookup is 6–8 ms, stored
  hydration is under 1 ms, and lexical verification is 2–3 ms. A request-wide
  design could collapse 24 statements to 3 without reducing row or content
  work, so address lexical query cost before restructuring hydration.
- Keep FTS `columnsize=1`. The OpenClaw A/B saved only 1.80% of database bytes
  with `columnsize=0`, while BM25-backed realistic context regressed from
  1,066 ms to 2,682 ms p50 and reproduced in reverse order. Do not trade stored
  token counts for on-demand retokenization.
- Keep full mandatory literals in regex trigram MATCH expressions. Dynamic
  `fts5vocab` frequency lookup cost 8–45 ms on OpenClaw, while rare trigram
  pairs expanded sparse and compound candidate sets by 4.4× and 12.4×.
  Reconsider only with generation-built frequency metadata and end-to-end
  candidate loading and verification evidence.
- Keep the current deterministic BM25 query. SQLite rank-first hydration
  preserved the top-128 set and order for four OpenClaw queries, but was faster
  for two and slower for two. A future ranking change must preserve frozen-task
  context coverage and avoided follow-up retrieval calls, not merely top-k
  overlap.
- Keep canonical-compatible token counting. Exact tokenization owned 57.6% of
  summed preparation worker time, but a 6.1–7.3× faster Rust implementation
  disagreed on 3,550 OpenClaw files and by as many as 1,483 tokens in one file.
  Pursue a no-allocation counter in the canonical-compatible implementation
  instead of changing token-budget semantics.
- `notify-debouncer-full` was evaluated as a replacement for the watcher state
  machine. Its file-ID rename pairing is useful, but its unbounded internal
  queue and blocking shutdown do not preserve LeanToken's bounded-overflow and
  cancellation-flush contracts. Keep the current conservative watcher unless
  native macOS traces show rename rescans are a material cost.

## Indexing efficiency

- Watcher events for known regular files now use targeted reconciliation.
  Correctness-sensitive cases fall back to full discovery.
- The synthetic release profile showed a lower one-file update cost for the
  targeted path at 2,000 files. Continue profiling real monorepos before adding
  more incremental-index machinery.
- The profiler now measures create, delete, rename, and ignore-control changes
  through the same path-reconciliation entry point used by watcher events.
  They now measure visibility deltas rather than unconditional rebuilds.
  Watcher delivery latency, overflow, and interrupted reconciliation remain
  separate stress measurements.
- A five-sample pinned-Tokio run first measured median create, rename, and
  ignore-change rebuilds at 21.1 s, 13.5 s, and 29.9 s. The implementation now
  stores every bounded import candidate in an indexed reverse projection
  and reparses only changed paths and importers affected by membership changes.
  On the same 865-file tree, medians fell to 226 ms,
  89 ms, and 49 ms; create indexed one file, rename indexed one and removed one,
  and a comment-only ignore change indexed only `.gitignore`. This addresses
  [#48](https://github.com/morluto/leantoken/issues/48) without a journal,
  shard layer, or cache.

## Reliability

- Keep exercising concurrent reads during reconciliation, queue overflow,
  rename ambiguity, large bounded requests, cancellation, EOF, corrupt-cache
  recovery, generation consistency, and Windows startup/shutdown in CI.
- Add host-specific disconnect traces and native Windows stress runs when the
  CI matrix reveals failures; do not simulate platform guarantees from Linux.

## Out of scope

Editing, command execution, persistent sessions, subagents, model routing,
embeddings, remote indexing, and a frontend are not planned for the retrieval
MVP. They should not be added merely to match a broader agent platform.

If editing is ever added, it needs expected hashes, unique replacements,
dry-run support, atomic writes, and synchronous index invalidation. If agent
execution is ever added, it should remain a separate orchestration layer so the
retrieval core stays model-independent.
