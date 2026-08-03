# Changelog

All notable changes to this project will be documented in this file.
## [0.1.21] - 2026-08-01
### Benchmarks

- Separate stable summaries from raw evidence
### Bug Fixes

- **release:** Format preserved changelog entries
- **release:** Preserve previous changelog entries
- **mcp:** Restore tool-local routing cues
- **protocol:** Restore public APIs and validate setup paths
- **retrieval:** Preserve bounded response and snapshot contracts
- **ci:** Align validation planning with changed artifacts
- **ci:** Close planner review gaps
- **test:** Make sandbox creation process-safe
- **setup:** Classify package-manager invocation identity
- **watcher:** Reconcile ambiguous rename events
- **read:** Preserve open bounded cursor targets
- **storage:** Keep staging databases out of repositories
- **benchmarks:** Update live-read policy references
- **benchmarks:** Qualify read policy imports
- **context:** Preserve empty-pattern validation errors
- **read:** Simplify cursor freshness checks
- Migrate graph benchmark to canonical diagnostics field
- **concurrency:** Align CPU-aware capacity bounds
- **setup:** Invoke npx launchers directly
- **tests:** Isolate process tests from inherited npm_lifecycle_event
- **upgrade:** State how to upgrade npx integrations
- **upgrade:** Avoid unrelated global install guidance
### Chores

- Prune orphaned benchmark artifacts
- Remove historical audit artifacts
- Format cleanup changes
### Continuous Integration

- Optimize test suite routing
### Documentation

- Document the new retrieval contracts
- **read:** Describe versioned continuation endpoints
- **architecture:** Document file-backed reconciliation staging
- **architecture:** Align staging ledger with implementation
- **architecture:** Record root-cause cleanup evidence
- **json:** Clarify operation and projection contracts
- Describe dynamic reader pool bounds
- Add comprehensive DX audit report (July 2026)
### Features

- **mcp:** Publish through Cargo and MCP Registry
- **setup:** Harden agent onboarding lifecycle (#431) ([#431](https://github.com/morluto/leantoken/pull/431))
- **context:** Expose bounded repository provenance
- **mcp:** Align contracts with RMCP 3.1.0
- **storage:** Stage reconciliation records in SQLite
- **read:** Make live freshness and I/O policy explicit
- **search:** Bound regex work and clarify occurrence routing
### Performance

- **test:** Scale process workers by runner
- **json:** Cache exact projection measurements
- **search:** Bound regex work by request budgets
- **indexer:** Move preparation outside publication transactions
- **json:** Reuse measured schema projections
- Make blocking executor and SQLite pool size CPU-aware
### Refactoring

- **repository:** Centralize relative path and pattern policy
- Remove retired compatibility layers
- **examples:** Model benchmark pipeline state
- Remove production clippy suppressions
- **services:** Stage context and search execution
- **json:** Decompose structural query service
### Testing

- **mcp:** Refresh generated catalog snapshot
- **read:** Request full freshness in live profile
- Simplify frozen report lint path
- Decouple frozen graph report from harness churn
- Refresh graph report provenance

## [0.1.20] - 2026-07-30
### Benchmarks

- **mcp:** Record receipt resource promotion gate
### Bug Fixes

- **ci:** Align product workspace feature graph
- **status:** Report active repository fallback
- Address MCP readiness review findings
- **mcp:** Survive read-only managed caches
- **eval:** Bound Kotlin evidence collection
- **eval:** Fail closed on Kotlin evidence gaps
- **eval:** Tighten Kotlin evidence accounting
- **ci:** Stabilize cross-platform benchmark fixtures
- **ci:** Clean benchmark dependencies and portable contracts
- **eval:** Preserve Kotlin manifest bytes on Windows
- **eval:** Normalize receipt-derived accounting
- **test:** Reject empty focused selections (#379) ([#379](https://github.com/morluto/leantoken/pull/379))
- **mcp:** Constrain exhaustive search modes
- Close post-merge review gaps
- **test:** Reject ambiguous selectors and malformed fixtures
- **ci:** Update test artifact uploads
- **ci:** Make AGENTS validation compile-free (#375) ([#375](https://github.com/morluto/leantoken/pull/375))
- **ci:** Complete portable test harness review fixes
- **test:** Address review feedback for portable harnesses
- **ci:** Make test sandboxes and alias checks portable
- **mcp:** Preserve receipt resource invariants
- **mcp:** Avoid cloning static result mode
- **search:** Keep planner fallback lint-clean
### Chores

- **dev:** Report target footprint (#380) ([#380](https://github.com/morluto/leantoken/pull/380))
### Continuous Integration

- Reject lockfile drift (#378) ([#378](https://github.com/morluto/leantoken/pull/378))
- Update Node 24 action majors (#376) ([#376](https://github.com/morluto/leantoken/pull/376))
- Allow recorded regex trial revision
### Documentation

- **measurement:** Record enclosing lookup evidence
- **eval:** Record Kotlin structural no-ship decision
- **eval:** Record Swift structural no-ship decision (#383) ([#383](https://github.com/morluto/leantoken/pull/383))
- Remove obsolete rmcp release warning (#377) ([#377](https://github.com/morluto/leantoken/pull/377))
### Features

- **setup:** Verify configured MCP launchers
- **eval:** Diagnose TypeScript parse recovery (#382) ([#382](https://github.com/morluto/leantoken/pull/382))
- **mcp:** Expose retrieval receipt resources
- **search:** Broaden bounded regex planning
- **retrieval:** Report named limit failures
- **index:** Report parser coverage
### Performance

- **ci:** Overlap independent product test lanes
- **ci:** Avoid rebuilding benchmark targets for fixtures
- **eval:** Prototype Kotlin structural indexing
- **ci:** Reuse test profile for fixtures (#381) ([#381](https://github.com/morluto/leantoken/pull/381))
### Refactoring

- Finish legacy module tree migration
- **errors:** Replace stringly failures and bound shutdown
- **benchmarks:** Move runners into opt-in package
- Replace organizational include module trees
### Testing

- **eval:** Add Python resolved-reference oracle
- **setup:** Cover launcher verification failures
- **storage:** Compare canonical fallback path
- **architecture:** Reject organizational includes outright
- **architecture:** Add immutable fixtures and include guard
- **eval:** Classify Kotlin parse gaps
- **eval:** Freeze Kotlin structural gate
- Remove no-op assertions
- Route CI through workspace test lanes
- Consolidate integration coverage by domain
- Add workspace-owned suite infrastructure
- **services:** Avoid queue timing in snapshot test
