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

## Token accounting

- Exact local modes now cover the bundled `tiktoken-rs` encodings; an explicit
  inexact estimate mode covers providers without a local vocabulary.
- MCP accounting includes initialization, the five-tool catalog,
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
- Representation tests compare context fragments, search excerpts, outlines,
  full reads, and a compact repository tree under visible source and complete
  JSON token counts.
- Add model input framing and provider-native counts where hosts expose them.
  Never silently substitute a local tokenizer for provider billing counts.

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
  Do not add call-path output until labeled precision and its protocol cost beat
  lexical retrieval on a holdout.
- Do not add a hot-file cache yet. The release profile measured warm live reads
  in tens of microseconds on this host; cache ownership, invalidation, and
  memory cost need end-to-end evidence first.
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
