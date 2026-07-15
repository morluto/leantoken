# Roadmap

LeanToken's roadmap is evidence-driven. A feature should reduce wasted model
reads or improve relevant-range recall before it expands the MCP tool surface.

## Retrieval quality

- Keep the eight pinned public-fix tasks split into a visible development set
  and add a holdout before making retrieval-quality claims.
- Continue improving useful-line recall. The benchmark now reports labeled
  line anchors, unlabeled-fragment cost, known-hash resends, an explicit
  line-proportional repeated-range token estimate, and full second-turn request
  and response cost.
- Task extraction now preserves qualified identifiers and header-like terms,
  reserves query slots for prose intent, round-robins identifier expansions,
  and fuses only independent explicit concepts. Symbol declaration reads now
  cross index-chunk boundaries. Eager lexical-to-declaration expansion was
  removed because it added indexed lookups without improving the measured
  selected ranges.
- Add a language grammar only when a pinned task and parser fixture demonstrate
  recall value that outweighs its binary, indexing, and schema cost. The
  expanded task set uses existing grammars, so no grammar was added.

## Token accounting

- Exact local modes now cover the bundled `tiktoken-rs` encodings; an explicit
  inexact estimate mode covers providers without a local vocabulary.
- MCP accounting now includes initialization, the real five-tool catalog,
  `notifications/initialized`, JSON-RPC envelopes, the SDK's duplicated
  text-plus-structured result, and a repeated-context handoff. Optional output
  schemas were removed from the catalog while structured results were kept;
  the fixture catalog is 1,364 tokens and the modeled handoff is 3,472 tokens.
- Representation tests compare context fragments, search excerpts, outlines,
  full reads, and a compact repository tree under visible source and complete
  JSON token counts.
- A future trace benchmark should add model input framing and provider-native
  token counting without weakening the distinction between exact and estimated
  counts.

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

## Out of scope

Editing, command execution, persistent sessions, subagents, model routing,
embeddings, remote indexing, and a frontend are not planned for the retrieval
MVP. They should not be added merely to match a broader agent platform.

If editing is ever added, it needs expected hashes, unique replacements,
dry-run support, atomic writes, and synchronous index invalidation. If agent
execution is ever added, it should remain a separate orchestration layer so the
retrieval core stays model-independent.
