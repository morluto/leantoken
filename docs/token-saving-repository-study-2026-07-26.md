# Token-Saving Repository Study

Date: 2026-07-26

LeanToken baseline: `v0.1.15` on `main`

This document records a source-level review of projects that reduce coding-agent
context or improve code retrieval. It updates the local 2026-07-16 research
package under `preparation/` against the current LeanToken implementation.

The goal is not to copy the largest feature set. The goal is to identify
mechanisms that can improve LeanToken's measured task success or token economy
without turning its eight read-only tools into a general agent platform.

## Executive conclusion

The strongest candidates are:

1. Import public retrieval corpora and baselines before changing production
   ranking.
2. Keep production retrieval lightweight; the proposed model-backed semantic
   lane was deliberately skipped after review.
3. Add generation-aware delta responses for repeated exact reads in an edit
   loop.
4. Improve the existing review workflow with semantic change classification and
   a compact, provenance-bearing handoff manifest.

The following should not be added to LeanToken core without new evidence:

- a large MCP tool surface;
- general shell-output compression;
- persistent project memory or editing;
- an always-on LSP/SCIP runtime;
- PageRank, reverse-dependency, or caller boosts by default;
- lossy entropy or regex-based source pruning;
- a mandatory embedding model or network service.

## Method

The review used four evidence layers:

1. The 2026-07-16 preparation package and its original experiments.
2. Current LeanToken source, tests, roadmap, and measurement reports.
3. Shallow clones of each candidate at the immutable revisions listed below.
4. Official repository documentation and project websites.

Marketing claims are treated as hypotheses. A feature is recommended only when
its implementation has a plausible owner in LeanToken and a measurable
acceptance gate.

Local clone root used during the review:

```text
/tmp/leantoken-reference-repos
```

## Reviewed snapshots

| Project | Revision | License observed | Primary topic |
| --- | --- | --- | --- |
| [Semble](https://github.com/MinishLab/semble) | `90631955` | MIT | Static embeddings + BM25 |
| [Sverklo](https://github.com/sverklo/sverklo) | `3156effb` | MIT | Multi-lane retrieval and diagnostics |
| [Aider](https://github.com/Aider-AI/aider) | `5dc9490b` | Apache-2.0 | Personalized repository map |
| [jCodeMunch](https://github.com/jgravelle/jcodemunch-mcp) | `19277b18` | Non-commercial/custom | Ranked context, change and session tools |
| [SDL-MCP](https://github.com/GlitterKill/sdl-mcp) | `28638657` | Community/custom | Provider-first graph and session deltas |
| [Serena](https://github.com/oraios/serena) | `34342a9d` | MIT | LSP-backed symbolic access |
| [Code Context Engine](https://github.com/elara-labs/code-context-engine) | `da47caca` | MIT | Vector/BM25 retrieval and memory |
| [grepai](https://github.com/yoanbernabeu/grepai) | `c4f294b3` | MIT | Semantic grep and optional hybrid search |
| [GitNexus](https://github.com/abhigyanpatwari/GitNexus) | `89bbdcf5` | PolyForm Noncommercial | Knowledge graph, PDG, context packs |
| [Repomix](https://github.com/yamadashy/repomix) | `f0968929` | MIT | Static repository packs |
| [RTK](https://github.com/rtk-ai/rtk) | `23d1e899` | Apache-2.0 | Command-aware output filtering |
| [Headroom](https://github.com/headroomlabs-ai/headroom) | `4bd12149` | Apache-2.0 | Reversible context compression |
| [lean-ctx](https://github.com/yvgude/lean-ctx) | `fe36c06e` | Apache-2.0 | Broad context gateway and memory |
| [SAGE](https://github.com/PsYcGoD/sage) | `c7b79184` | MIT | Command output summary + recovery |

Revision identifiers are deliberately recorded because these projects move
quickly and their documentation, benchmarks, and licenses may change.

## Current LeanToken baseline

Much of the 2026-07-16 priority list has already landed. The current product
already has:

- context query plans and `plan_only`;
- hard path scopes, `max_fragments`, must-cover constraints, and coverage
  diagnostics;
- exact occurrence search with returned/total counts;
- outlines for Markdown, HTML, CSS, JavaScript, and TypeScript;
- source-token and payload-token accounting;
- generated-artifact and nested Git-metadata exclusion;
- structural JSON queries;
- immutable symbol history;
- diff scopes, changed hunks, changed symbols, related paths, and owner-test
  candidates;
- bounded server-managed receipts for repeated evidence;
- channel, facet, and path provenance in ranking diagnostics;
- frozen retrieval evaluations, exact MCP wire capture, and model-in-the-loop
  harnesses.

The relevant implementation owners are:

- [`src/services/context.rs`](../src/services/context.rs) for query facets,
  ranking channels, RRF-style corroboration, workflow routing, and diff scopes;
- [`src/services/receipts.rs`](../src/services/receipts.rs) for bounded
  repeated-evidence suppression;
- [`src/model.rs`](../src/model.rs) for coverage, omission, diff, routing, and
  token-cost contracts;
- [`docs/measurement.md`](measurement.md) and
  [`benchmarks/README.md`](../benchmarks/README.md) for promotion evidence;
- [`docs/roadmap.md`](roadmap.md) for negative graph evidence and product
  boundaries.

This means several attractive features from other projects are already present
in a smaller form. They should be refined rather than reintroduced as new MCP
tools.

## Retrieval systems

### Semble

Semble is the most credible small semantic-retrieval candidate reviewed.

Its search pipeline is:

1. Tree-sitter code-aware chunks.
2. Static Model2Vec embeddings using `potion-code-16M`.
3. BM25 lexical search.
4. Weighted reciprocal-rank fusion.
5. Query-shape-aware lexical/semantic weighting.
6. Definition, identifier, filename, and file-coherence boosts.
7. Path and same-file saturation penalties.

The implementation is small enough to isolate experimentally. It does not
require a GPU, API key, or transformer forward pass at query time.

Its published benchmark covers 1,251 queries over 63 repositories and 19
languages. Reported NDCG@10 is 0.854. The ablation is especially useful:
ranking heuristics contribute a large part of the result, not embeddings alone.

Important limitations:

- queries and relevance labels were generated by Claude Sonnet 4.6;
- the same model judged label quality;
- the token baseline is grep plus matched-file reads, not current LeanToken;
- generic penalties for tests, examples, compatibility code, or declarations
  can hurt tasks that explicitly target those files;
- current Semble invalidation rebuilds the index rather than demonstrating the
  same incremental storage contract as LeanToken.

Decision: import the dataset or an adapter first. Only then test a semantic lane
inside LeanToken.

Relevant source:

- [`search.py`](https://github.com/MinishLab/semble/blob/90631955/src/semble/search.py)
- [`boosting.py`](https://github.com/MinishLab/semble/blob/90631955/src/semble/ranking/boosting.py)
- [benchmark methodology](https://github.com/MinishLab/semble/blob/90631955/benchmarks/README.md)

### Sverklo

Sverklo is useful less as a product shape and more as a retrieval-diagnostics
reference.

Useful mechanisms:

- named FTS, vector, path, symbol, and documentation lanes;
- lane hit counts, overlap, candidate-pool size, and missing-vector reporting;
- explicit confidence and fallback guidance;
- an "enoughness" receipt that distinguishes shown bodies, location-only
  evidence, hidden evidence, and proof gaps;
- filename-as-signal and sibling-definition expansion;
- an optional late-interaction reranker that fails open;
- token-budget packing with explicit overflow.

One implementation detail changes how its semantic claims should be
interpreted: vector scoring scans a bounded pool derived from FTS hits, sibling
chunks, and high-PageRank files. It is not a global semantic recall lane for
lexically disconnected files.

The public benchmark reports 180 hand-verified tasks across six repositories
and includes loss slices. That is useful external evidence, but it should still
be replayed through a LeanToken adapter rather than used to select production
weights.

The 37-tool product surface, persistent memory, and direct PageRank boost do not
fit LeanToken core.

Relevant source:

- [`hybrid-search.ts`](https://github.com/sverklo/sverklo/blob/3156effb/src/search/hybrid-search.ts)
- [`enoughness.ts`](https://github.com/sverklo/sverklo/blob/3156effb/src/search/enoughness.ts)
- [`rerank.ts`](https://github.com/sverklo/sverklo/blob/3156effb/src/search/rerank.ts)

### Code Context Engine

CCE is valuable because it publishes both a strong result and a serious
failure.

Its hybrid retriever performs global vector search, FTS hydration, RRF,
confidence blending, path penalties, overlap deduplication, per-file caps,
graph expansion, and token-budget packing.

Published Recall@10:

| Repository | Shape | Recall@10 |
| --- | --- | ---: |
| FastAPI | 53 Python source files | 0.90 |
| chi | 94 Go files | 0.67 |
| Fiber | 396-file Go monorepo, 4,382 chunks | 0.07 |

The Fiber result is the key lesson. A semantic lane can look strong on a
single-framework Python corpus and fail badly when a monorepo contains many
similarly shaped packages and symbols.

Other cautions from the source:

- tests, docs, specs, and plans receive a generic path penalty;
- recency affects semantic confidence even when code age is not relevance;
- graph expansion appends bonus chunks outside the main ranked list;
- the headline 94% savings baseline is full-file reads, not actual agent
  exploration.

Decision: use CCE's failure strata in the acceptance suite. Do not adopt its
weights or graph expansion.

Relevant source:

- [`retriever.py`](https://github.com/elara-labs/code-context-engine/blob/da47caca/src/context_engine/retrieval/retriever.py)
- [published benchmark results](https://github.com/elara-labs/code-context-engine/tree/da47caca/benchmarks/results)

### grepai

grepai confirms the common minimum design: semantic retrieval, optional keyword
search, RRF, path boosts, and result deduplication.

Its current text lane scans chunks in memory and scores matched query words.
Documentation explicitly warns about large indexes. This is a useful simple
baseline, but it does not add a mechanism beyond LeanToken's current lexical
and structural lanes.

Decision: baseline only.

## Structural and semantic systems

### Aider repository map

Aider builds definition/reference tags with Tree-sitter, creates a
referencer-to-definer file graph, and uses personalized PageRank. The
personalization is the important part:

- current chat files and explicitly mentioned files receive more weight;
- mentioned or distinctive identifiers receive more edge weight;
- private and overly common definitions receive less;
- the selected signatures are fitted to a token budget.

This is more defensible than unconditional global PageRank because the prior is
conditioned on the active task.

LeanToken's frozen graph ablation found no recall lift from reverse
dependencies and only one additional line anchor from parsed caller candidates,
with large false-positive pools. Therefore, an Aider-style map belongs only in
an isolated cold-start or `plan_only` ablation. It must not become a default
ranking boost without positive evidence.

Relevant source:

- [`repomap.py`](https://github.com/Aider-AI/aider/blob/5dc9490b/aider/repomap.py)
- [official repo-map description](https://aider.chat/docs/repomap.html)

### Serena

Serena provides precise, on-demand semantic operations backed by language
servers or JetBrains:

- symbol overview and body retrieval;
- definition, implementation, and reference lookup;
- diagnostics;
- symbolic edits and refactors.

The strongest lesson is progressive semantic access: obtain an outline, select
one symbol, then request its body or references. LeanToken already follows that
interaction shape with `outline`, `search`, and `read`.

An LSP lane would improve overloads, dispatch, definitions in some languages,
and compiler-grade references. It would also introduce language-server
installation, startup, process isolation, project configuration, stale-server
handling, partial language support, and substantial cross-platform cost.

Decision: prefer interoperability or a separately executable experiment over
embedding LSP lifecycle management in LeanToken. Promote only if frozen tasks
demonstrate a Tree-sitter precision gap that affects task success.

### GitNexus

GitNexus explores the far end of graph-based code intelligence: scope
resolution, hybrid search, communities, processes, impact analysis, and a
program-dependence graph.

The most reusable idea is not the graph. It is the implementation context-pack
contract:

- task summary and acceptance criteria;
- exact files and symbols to modify;
- tests and verified commands;
- assumptions, open questions, risks, and explicit "avoid" constraints;
- commit and working-tree provenance;
- a bounded manifest instead of repeated discovery or copied source.

This resembles a durable, cross-session form of LeanToken's current receipts,
diff receipt, query plan, and coverage diagnostics.

Decision: independently design a small handoff manifest derived from existing
LeanToken evidence. Do not copy GitNexus source: the reviewed revision is
PolyForm Noncommercial.

### jCodeMunch

jCodeMunch contains many relevant ideas, but its breadth makes individual
mechanisms more useful than its product shape.

Useful mechanisms:

- BM25, PageRank, exact query seeding, multi-channel RRF, and diversity packing;
- git revision comparison at symbol granularity;
- a session journal containing files read, searches, edits, negative evidence,
  and tool counts;
- a compact pre-compaction snapshot of focus files and dead ends;
- confidence, freshness, negative-evidence, and ranking-event telemetry.

The `entropy_prune` path scores lines using Shannon entropy and preserves
regex-selected "keystone" lines. This can reduce output but cannot guarantee
that contracts, invariants, or subtle control flow survive. It is unsuitable as
a default or authoritative source representation.

Decision:

- learn from the session snapshot and semantic diff;
- do not add its large tool catalog;
- do not adopt entropy pruning except as a reversible model A/B experiment;
- do not copy source because the reviewed license is non-commercial/custom.

Relevant source:

- [`get_ranked_context.py`](https://github.com/jgravelle/jcodemunch-mcp/blob/19277b18/src/jcodemunch_mcp/tools/get_ranked_context.py)
- [`get_changed_symbols.py`](https://github.com/jgravelle/jcodemunch-mcp/blob/19277b18/src/jcodemunch_mcp/tools/get_changed_symbols.py)
- [`get_session_snapshot.py`](https://github.com/jgravelle/jcodemunch-mcp/blob/19277b18/src/jcodemunch_mcp/tools/get_session_snapshot.py)
- [`entropy_prune.py`](https://github.com/jgravelle/jcodemunch-mcp/blob/19277b18/src/jcodemunch_mcp/retrieval/entropy_prune.py)

### SDL-MCP

SDL-MCP provides two important reference designs.

First, its provider-first indexing separates:

- provider-neutral semantic facts;
- provider and model identity;
- coverage denominators;
- exact versus heuristic edges;
- readiness and safety gates;
- same-run Tree-sitter fallback for uncovered files.

Any future LeanToken semantic, LSP, or SCIP lane should make those properties
visible instead of silently blending partial evidence.

Second, its session-delta logic addresses a real LeanToken gap:

- first exact read returns full content;
- an unchanged repeat returns no content;
- a small change returns a bounded unified diff;
- large, truncated, or unsafe cases return full content;
- base/head hashes and exact avoided-token counts are reported;
- state is session-scoped and bounded by TTL, entry count, and bytes.

LeanToken receipts currently reject a receipt when repository generation
changes. They suppress repeated evidence within one generation but cannot
describe what changed after an edit.

Decision: independently implement and test generation-aware exact-read deltas.
Do not copy SDL-MCP source because its Community License restricts commercial
distribution.

Relevant source:

- [`session-delta.ts`](https://github.com/GlitterKill/sdl-mcp/blob/28638657/src/mcp/session-delta.ts)
- [`session-dedupe.ts`](https://github.com/GlitterKill/sdl-mcp/blob/28638657/src/mcp/session-dedupe.ts)
- [`provider-first`](https://github.com/GlitterKill/sdl-mcp/tree/28638657/src/indexer/provider-first)

## Packing and output compression

### Repomix

Repomix is a strong static-pack baseline:

- full, structure-only, or omitted content per path pattern;
- Tree-sitter signature extraction;
- repository tree and per-file token-count tree;
- output splitting;
- secret scanning and remote-config trust boundaries;
- fail-open full content when compression cannot parse a file.

LeanToken's interactive outline/search/read flow already provides more precise
progressive disclosure than a whole-repository pack. Repomix remains useful as
a comparison arm for cold-start orientation and as a reminder that output
policy may vary by content family.

Its secret checks expose an adjacent safety question. LeanToken respects
`.gitignore` and rejects ignored reads, but it does not independently scan
tracked content for credentials. This should be evaluated as a separate
security feature, not bundled into retrieval ranking.

Decision: retain as a benchmark baseline; consider a separate sensitive-path
diagnostic proposal.

### RTK and SAGE

RTK and SAGE wrap shell commands and return compact, command-aware output.
Their strongest design properties are:

- delegate to the exact command or search engine requested;
- preserve exit semantics, including non-error "no match" exits;
- show failures or raw output when compression is unsafe;
- keep bounded raw output for recovery;
- count savings from actual input and returned output.

This is valuable at the coding-agent host layer. It is outside LeanToken's
read-only repository-retrieval boundary.

Decision: do not add shell execution or command filtering to LeanToken. A host
integration may recommend these tools independently.

### Headroom

Headroom routes JSON, logs, diffs, search output, code, and prose through
content-specific compressors. It keeps originals in a reversible content
reference store and aligns stable prompt prefixes for provider cache hits.

Useful principles:

- compress only when the result is materially smaller;
- fail open on parse, latency, or quality failure;
- preserve omission metadata and an exact recovery path;
- evaluate answer quality, not token reduction alone.

LeanToken evidence already has natural recovery coordinates: repository, path,
line range, content hash, and receipt. A second general raw-content store would
duplicate that property.

Decision: adopt the fail-open and net-benefit principles, not the proxy,
conversation-memory, or general compression platform.

### lean-ctx

lean-ctx combines code retrieval, graph queries, memory, handoff, shell
compression, recoverable references, and a large MCP surface.

Its cached rereads, handoff manifest, PR packs, and token ledger are useful
ideas. Its broad gateway architecture is also the clearest example of the scope
LeanToken should avoid.

Decision: keep LeanToken's eight-tool surface and use existing response
contracts for new behavior.

## Recommended experiments

### P0: external retrieval corpus adapter

Add a benchmark-only adapter that can replay:

- Semble's 1,251-query, 19-language corpus;
- Sverklo's 180-task corpus, subject to its dataset license;
- CCE's monorepo and short-file failure shapes;
- current LeanToken frozen multilingual tasks.

Record immutable repository revisions and evaluate:

- file recall and line-anchor recall;
- NDCG and first-relevant-result rank;
- tokens to first relevant evidence;
- source, payload, and total response tokens;
- p50 and p95 query latency;
- index time and disk footprint;
- exact-identifier, natural-language, test-targeting, configuration, monorepo,
  and same-name-symbol strata.

The adapter must not change production ranking.

### P0: local semantic lane experiment (skipped)

Prototype a separately gated static embedding lane, starting with Semble's
Model2Vec approach.

Decision on 2026-07-26: skip this experiment. Even a local Model2Vec lane adds
model artifacts, lifecycle, index footprint, and runtime policy that do not fit
the current lightweight product boundary. No branch, tracked artifact, or PR was
retained. The contract below remains as historical evaluation criteria if that
boundary is revisited.

Required contract:

- disabled by default;
- no network request at query time;
- model name, revision, dimensionality, and content fingerprint in diagnostics;
- explicit indexed/eligible/missing chunk counts;
- semantic-only, lexical-only, overlap, and selected-result contribution;
- deterministic RRF integration with current facets and channels;
- exact/symbol lanes retain priority for code-shaped queries;
- no generic test or documentation penalty;
- fail-open lexical/structural behavior if the model is unavailable.

Promotion gate:

- no regression on exact identifier and hard-scope tasks;
- positive file and line recall at fixed payload budgets on a separately frozen
  development set;
- no Fiber-like monorepo collapse;
- bounded index and runtime footprint;
- a separately executable model-in-the-loop result before default enablement.

### P1: generation-aware exact-read delta

Extend exact `read` behavior or its receipt contract:

1. First read returns full content and a stable target key.
2. Same target and same hash returns `not_modified`.
3. Same target with a new hash may return a bounded unified diff when it is
   strictly cheaper than full content.
4. Missing base state, changed coordinates, truncation, binary/invalid UTF-8,
   or an oversized diff returns full content.
5. Response reports base/head hashes, full tokens, delta tokens, avoided tokens,
   and the fallback reason.
6. State is bounded by receipt/session, TTL, entry count, and bytes.

Start with exact line or symbol reads. Do not apply deltas to ranked context
fragments until exact-read behavior is proven.

Acceptance should use repeated edit-read-fix-test workflows and actual model
task success, not synthetic diff size alone.

### P1: semantic change receipt

The current `DiffEvidenceReceipt` identifies changed hunks, symbols, and related
paths, but does not classify how a symbol changed.

Experiment with bounded revision-to-revision classification:

- added, removed, renamed, or modified;
- signature changed versus body-only changed;
- public contract or configuration key changed;
- owner tests found or missing;
- coverage gaps stated explicitly.

Avoid speculative "risk scores" until individual signals are validated.
Existing `context` review workflow and `history` should own this behavior; no new
MCP tool is needed.

Implemented and adopted in `semantic-change-receipt-v1` as a bounded,
model-free review receipt. See
`benchmarks/reports/semantic-change-receipt-v1-2026-07-26.md` for the exact
fixture gate, payload cost, latency, and limitations.

### P2: provenance-bearing handoff manifest

Derive a compact manifest from existing query plans, receipts, diff receipts,
and validation evidence:

- task summary;
- repository and commit identity;
- focused paths and symbols with content hashes;
- changed and related paths;
- tests and verified commands;
- assumptions, open questions, negative evidence, and explicit avoid rules.

The manifest must contain coordinates and hashes, not copied full files. It
should be host-triggered for compaction or agent handoff, not an always-on
persistent memory system.

### P2: optional semantic-provider boundary

Define an interface experiment for LSP or SCIP facts without owning the full
server lifecycle:

- provider identity and version;
- language and path coverage;
- freshness and repository revision;
- exact versus heuristic facts;
- explicit fallback and missing subsets.

Only proceed if frozen tasks show a material gap in Tree-sitter symbol
resolution or reference precision.

## Explicit non-goals

| Feature | Decision | Reason |
| --- | --- | --- |
| Default PageRank/caller boost | Reject for now | Current ablation shows high false-positive rates and no reliable recall lift |
| General embeddings in default install | Defer | Footprint and monorepo quality are unproven |
| Entropy/regex source pruning | Reject as authoritative output | Cannot preserve semantic invariants |
| Whole-repository pack as main workflow | Reject | Interactive retrieval is more precise and recoverable |
| Shell/test/log compression | Host layer | Not repository retrieval |
| Persistent decisions or chat memory | Host layer | Changes product ownership and privacy model |
| Editing/refactoring tools | Out of scope | LeanToken is read-only |
| 30-80 MCP tools | Reject | Schema cost, overlap, and routing complexity |
| Always-on LSP/SCIP | Defer | Heavy lifecycle and cross-platform burden |
| Copying jCodeMunch, SDL-MCP, or GitNexus code | Prohibited | Reviewed licenses are not permissive for this use |

## Suggested branch sequence

If the experiments are approved, use separate branches and evidence-bearing
pull requests:

1. `bench/external-retrieval-corpora`
2. `feat/read-receipt-delta`
3. `feat/semantic-lane-experiment`
4. `feat/semantic-change-receipt`
5. `feat/handoff-manifest`

The semantic-lane branch should remain experimental unless its promotion gates
pass. The benchmark adapter should land first so later PRs cannot select only
favorable examples.

## Questions to resolve before implementation

1. Can the Semble and Sverklo benchmark datasets be redistributed or only
   downloaded by a benchmark script?
2. Should an embedding experiment be an optional Cargo feature, a separate
   binary/example, or an external adapter process?
3. Which host identity should scope exact-read delta state when an MCP client
   does not expose a stable session identifier?
4. Should delta output be a new response mode or a backward-compatible optional
   field in `ReadResponse`?
5. Is a handoff manifest emitted by LeanToken, or assembled by the host from
   existing receipts?
6. Should tracked credential-file detection be a warning, hard exclusion, or
   separate audit command?

## Primary references

- [Semble repository and benchmarks](https://github.com/MinishLab/semble)
- [Sverklo repository and public benchmark](https://github.com/sverklo/sverklo)
- [Aider repository-map documentation](https://aider.chat/docs/repomap.html)
- [Serena repository](https://github.com/oraios/serena)
- [Code Context Engine benchmark](https://elara-labs.github.io/code-context-engine/blog/benchmark-fastapi.html)
- [jCodeMunch repository](https://github.com/jgravelle/jcodemunch-mcp)
- [SDL-MCP documentation](https://glitterkill-sdl-mcp.mintlify.app/)
- [GitNexus repository](https://github.com/abhigyanpatwari/GitNexus)
- [Repomix compression documentation](https://repomix.com/guide/code-compress)
- [RTK repository](https://github.com/rtk-ai/rtk)
- [Headroom repository](https://github.com/headroomlabs-ai/headroom)
