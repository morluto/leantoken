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
- Expand the evaluation across more languages and task shapes before making
  broad retrieval claims. Record dead-end source, repeated ranges, known-hash
  resends, and complete two-turn cost alongside recall.
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
- Representation tests compare context fragments, search excerpts, outlines,
  full reads, and a compact repository tree under visible source and complete
  JSON token counts.
- Add model input framing and provider-native counts where hosts expose them.
  Never silently substitute a local tokenizer for provider billing counts.

## Model behavior

- Run the isolated four-arm A/B harness on repeated tasks: filesystem,
  progressive retrieval, one-shot context, and prewalk handoff.
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

## Indexing efficiency

- Watcher events for known regular files now use targeted reconciliation.
  Correctness-sensitive cases fall back to full discovery.
- The synthetic release profile showed a lower one-file update cost for the
  targeted path at 2,000 files. Continue profiling real monorepos before adding
  more incremental-index machinery.

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
