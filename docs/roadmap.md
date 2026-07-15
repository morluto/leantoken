# Roadmap

LeanToken's roadmap is evidence-driven. A feature should reduce wasted model
reads or improve relevant-range recall before it expands the MCP tool surface.

## Retrieval quality

- Grow the pinned task set and report range-level recall, dead-end reads, and
  repeated-reading cost.
- Improve task-term extraction, query fusion, and declaration-range selection,
  especially where retrieval finds the right file but misses the useful lines.
- Add language grammars only with parser fixtures and bounded schema impact.
- Reconcile known changed paths directly when measurements show that full
  metadata scans cause meaningful latency.

## Token accounting

- Add tokenizer modes for model families that cannot use `cl100k_base` exact
  counts.
- Measure complete tool-result tokens in real agent traces, including tool
  schemas and model handoffs, rather than optimizing source text alone.
- Evaluate compact repository maps and alternate fragment representations
  under the same recall and budget tests.

## Optional context signals

- Evaluate bounded working-tree diff and recent-change signals without turning
  LeanToken into a general Git client.
- Evaluate dependency and call-path views only after their syntax-only
  precision and token cost are measured.
- Consider a bounded hot-file cache only if filesystem reads appear in agent
  latency profiles.

## Out of scope

Editing, command execution, persistent sessions, subagents, model routing,
embeddings, remote indexing, and a frontend are not planned for the retrieval
MVP. They should not be added merely to match a broader agent platform.

If editing is ever added, it needs expected hashes, unique replacements,
dry-run support, atomic writes, and synchronous index invalidation. If agent
execution is ever added, it should remain a separate orchestration layer so the
retrieval core stays model-independent.
