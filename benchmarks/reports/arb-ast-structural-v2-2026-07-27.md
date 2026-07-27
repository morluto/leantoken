# ARB AST structural lane v2

Date: 2026-07-27

## Decision

Keep the v2 experiment and its bounded owner-evidence contract. The two-task
local ARB smoke result is a measured retrieval win over v1, but it is not broad
enough to promote the behavior into production ranking yet.

V1 already found the correct Clap and Click owner paths, but its two soft focus
paths did not change selected recall. V2 instead ranks source-language owner
paths with qualified-owner and named-argument corroboration, then appends one
exactly bounded definition excerpt without boosting the rest of the file.

## Frozen inputs

- Dataset: two-task local ARB v2 trace2code smoke adapter output.
- Manifest BLAKE3:
  `888fd766be72d8831946cf0038cf39374b5480564088f8d2cf7aa8d553bb6a7f`.
- Baseline: `--workflow-evidence --ast-structural-lane`.
- Candidate: `--workflow-evidence --ast-structural-lane-v2`.
- Both arms used the same current harness tree and already-local pinned Clap
  and Click checkouts.
- No corpus, repository, or history download was performed.

## V2 contract and bounds

The candidate:

1. reads at most 16 KiB of observed failure trace and two declared languages;
2. keeps at most eight AST/member terms, four qualified-owner terms, and four
   named-argument or object-field terms;
3. rejects whitespace-separated pseudo-qualifiers, file extensions, test
   names, and common value/type noise from the added lexical lane;
4. issues at most one bounded indexed search per retained term, with at most
   16 hits and 1,024 response tokens per search;
5. only lets auxiliary hits corroborate paths already found by structural
   definition search when the auxiliary symbol co-occurs with an owner range;
6. prefers paths whose extension matches the declared source language before
   deterministic co-occurrence, hit, score, and lexical tie-breaks;
7. emits at most two diagnostic owner paths, skips inexact owners, and reserves
   the first eligible definition excerpt at no more than 128 exact source
   tokens;
8. subtracts the owner excerpt from the same task source budget, keeps native
   candidate metrics separate, and suppresses the sidecar by exact content hash
   after the first turn.

Gold paths and spans are only applied after retrieval to score the result. The
experiment changes no production parser, index, storage schema, or ranking.

## Paired result

| Metric | V1 control | V2 candidate | Delta |
| --- | ---: | ---: | ---: |
| Selected labeled files | 1 / 3 | 2 / 3 | +1 |
| Selected file recall | 33.3% | 66.7% | +33.3 pp |
| Generated labeled files | 2 / 3 | 2 / 3 | 0 |
| Source tokens | 1,255 | 1,461 | +206 |
| Complete response JSON tokens | 2,698 | 2,922 | +224 |
| Dead-end source tokens | 1,054 | 1,054 | 0 |
| Two-turn JSON tokens | 7,729 | 7,821 | +92 |
| Exact-hash resends | 0 | 0 | 0 |

The two reserved owner excerpts were both relevant:

- Clap: `clap_builder/src/builder/arg.rs`, `default_value_if`, 78 source
  tokens.
- Click: `src/click/core.py`, `Option`, 128 source tokens.

The candidate made six bounded searches for Clap and twelve for Click. Both
owner reservations stayed inside their 2,000-token task budgets, were included
in the progressive known-hash request, and were not serialized or charged on
the second turn. A second complete candidate run produced identical terms,
owner paths, excerpts, hashes, recall, source-token counts, serialized-token
counts, dead-end cost, and two-turn cost.

## Interpretation

The AST lane is useful, but the useful contract is not “add more search
results.” Its value is routing one small, source-backed owner excerpt into
context. Compared with v1, this recovered Click's previously omitted owner
without adding dead-end source. The stricter progressive accounting records a
92-token two-turn premium: the owner payload is sent once, while its two exact
hashes remain visible in the follow-up request.

The next promotion experiment should freeze a broader multilingual task set and
test this exact reservation contract end to end with an editing agent. The
production form should remain opt-in until it preserves task success, relevant
recall, deterministic bounds, and total trajectory cost beyond this smoke set.
