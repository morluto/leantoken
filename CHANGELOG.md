# Changelog

All notable changes to this project will be documented in this file.
## [0.1.16] - 2026-07-26
### Benchmarks

- Record agent wall-time baseline
- Add agent wall-time A/B harness
- Record context concept coverage baseline
- Measure context concept coverage
- Record handoff manifest crossover
- Tighten exact-read delta accounting
- Record exact-read delta results
- Tighten external corpus contracts
- Record external corpus baseline
- Add pinned external retrieval corpora
- Add repository-scale performance diagnostics
### Bug Fixes

- Normalize release changelog ending ([#292](https://github.com/morluto/leantoken/pull/292))
- Restore release changelog generation ([#291](https://github.com/morluto/leantoken/pull/291))
- Make nested CLI help actionable
- Remove unusable CLI read delta flag
- Preserve tail intent in context queries
- **mcp:** Release dispatch capacity on cancellation
- **mcp:** Bound protocol lifecycle and redact payloads
- **setup:** Sync published directory entries
- **indexing:** Bound watcher and Git subprocess work
- Reconcile live CLI retrievals
- Isolate incompatible managed indexes
- Keep legacy cache age stable
- Register MCP initialization waiters
- Close MCP readiness races
- Bound idle initial-index waits
- Keep initial index retries inside MCP
- Drain benchmark MCP diagnostics
- Report MCP refresh after upgrades
- Support history symbol endpoints
- Report required symbol completeness
- Keep explicit changed paths authoritative
- Enforce read delta economy bounds
### Chores

- **dev:** Streamline local validation
- Add repository code owner
- Normalize changelog ending
### Continuous Integration

- Bound and isolate required checks
- Separate benchmark contract from product tests
### Documentation

- Clarify token savings methodology
- Update README hero asset path
- Refresh README hero image
- Improve README onboarding
- Record bounded runtime concurrency decisions
- Record lightweight semantic boundary
- Record semantic receipt decision
- Streamline repository agent guidance
### Features

- Expose index compatibility diagnostics ([#290](https://github.com/morluto/leantoken/pull/290))
- Bound and filter cache list output
- Add JSON diagnostics pagination
- Compact context diagnostics
- Add effective savings accounting
- **parser:** Add C# structural indexing
- Add provenance-bearing handoff manifests
- Add semantic change receipts
- Add accuracy-first optimization skill
- Add bounded exact-read deltas
- **search:** Prefilter regex candidates with trigram FTS
### Performance

- **mcp:** Back off follower leadership probes
- **read:** Fuse live hashing and range extraction
- **context:** Eliminate repeated retrieval scans
### Refactoring

- **services:** Bound blocking work and reconcile waves
### Testing

- Make executor waits scheduler-independent
- Stabilize MCP failover timing
- Prevent blocking executor teardown hangs
- Avoid Windows instant underflow
