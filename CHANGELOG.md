# Changelog

All notable changes to this project will be documented in this file.
## [0.1.28] - 2026-08-31
### Bug Fixes

- **mcp:** Expose receipt links as native resources
- **ci:** Restore main branch CI lanes
- Format tree and suppress cognitive_complexity on dispatch
- **cli:** Reject repository-scoped options on episode audit
- **storage:** Harden persisted index scope reuse
- **doctor:** Preserve broad root approval
- **doctor:** Forward repository indexing limits
- **doctor:** Preserve scoped index configuration
- **cli:** Disclose index scope for follow-up commands
- **mcp:** Clarify bounded search recovery
### Documentation

- Improve GitHub issue and pull request templates
### Features

- **cli:** Add --policy flag to read command
- **cli:** Close MCP/CLI parity gaps
### Refactoring

- **cli:** Remove duplicate update command, keep upgrade only
- **indexer:** Centralize scope response metadata
### Testing

- **mcp:** Follow native receipt resource links

## [0.1.27] - 2026-08-20
### Bug Fixes

- Harden MCP cancellation ID retention and coordination sidecar opens (#589) ([#589](https://github.com/morluto/leantoken/pull/589))
- Reject symlinked setup config and warn on changed-path truncation (#587) ([#587](https://github.com/morluto/leantoken/pull/587))
- Target MCP lifecycle stress, preserve focused-test ambiguity checks, and apply startup timeouts before repository-binding verification (#591) ([#591](https://github.com/morluto/leantoken/pull/591))
- Architectural improvements — watcher reconciliation, Go imports, receipt disambiguation, cache LRU (#590) ([#590](https://github.com/morluto/leantoken/pull/590))
- **package:** Include tests and README hero image in published crate
- Bound upgrade probes, align MCP schemas, preserve finite JSON medians and schema-diff completeness, reject setup-only remove flags, and harden release tagging (#592) ([#592](https://github.com/morluto/leantoken/pull/592))
- **upgrade:** Add --locked to cargo install for reproducible builds (#588) ([#588](https://github.com/morluto/leantoken/pull/588))
- **receipts:** Refresh query receipt LRU and TTL on reuse and handle response-accounting token cycles conservatively (#582) ([#582](https://github.com/morluto/leantoken/pull/582))
- **receipts:** Preserve the source receipt's existing LRU position after rebase (#583) ([#583](https://github.com/morluto/leantoken/pull/583))
- **upgrade:** Select Cargo releases by parsed SemVer (#581) ([#581](https://github.com/morluto/leantoken/pull/581))
- **setup:** Reject duplicate JSONC keys before editing (#584) ([#584](https://github.com/morluto/leantoken/pull/584))
- Correct read/outline/history tool description examples to match schema (#586) ([#586](https://github.com/morluto/leantoken/pull/586))
- **setup:** Use text results for Codex registrations (#580) ([#580](https://github.com/morluto/leantoken/pull/580))
- Fix exact npm upgrade pins, aggregate JSON diff budgets, Git diff error propagation and quoted paths, foreign SQLite rejection, empty-file setup recovery, and normalized required-evidence matching (#577) ([#577](https://github.com/morluto/leantoken/pull/577))
### Continuous Integration

- Make test evidence authoritative and single-scheduler (#593) ([#593](https://github.com/morluto/leantoken/pull/593))

## [0.1.26] - 2026-08-13
### Bug Fixes

- **ci:** Refresh gitleaks fingerprint for re-added regex trial report
- Enforce exact context excerpt caps (#573) ([#573](https://github.com/morluto/leantoken/pull/573))
- Generalize context owner selection (#571) ([#571](https://github.com/morluto/leantoken/pull/571))
- Derive query receipt semantics fingerprint from computation (#569) ([#569](https://github.com/morluto/leantoken/pull/569))
- Verify FTS index integrity on database open (#570) ([#570](https://github.com/morluto/leantoken/pull/570))
- Warn when process-wide indexing workers exceed CPU capacity (#568) ([#568](https://github.com/morluto/leantoken/pull/568))
- Neutralize Git pathspec magic and external diff helpers (#567) ([#567](https://github.com/morluto/leantoken/pull/567))
### Features

- Index C call references (#575) ([#575](https://github.com/morluto/leantoken/pull/575))
- Expose reference search capability (#574) ([#574](https://github.com/morluto/leantoken/pull/574))
### Refactoring

- Make retrieval and index invariants authoritative (#576) ([#576](https://github.com/morluto/leantoken/pull/576))
- Make repository generations the retrieval boundary (#572) ([#572](https://github.com/morluto/leantoken/pull/572))
- **test:** Consolidate redundant unit tests and tighten CI topology ownership (#478) ([#478](https://github.com/morluto/leantoken/pull/478))
- Replace boolean domain flags with typed enums (#479) ([#479](https://github.com/morluto/leantoken/pull/479))

## [0.1.25] - 2026-08-09
### Bug Fixes

- Checkpoint startup repair writes
- Avoid no-change reconcile amplification
- Reserve relevant context owners
- Keep mcp descriptions within budget
- Clarify setup runtime versions
- Satisfy context facet lints
- **ci:** Bound Windows process tests
- **ci:** Validate the published npm artifact
- **index:** Retain safe reconciliation state
- **retrieval:** Preserve bounded MCP pagination and errors
- **setup:** Preserve committed recovery state
- **search:** Preserve Unicode case-fold matches
- **read:** Reject zero-progress token pages
- **context:** Preserve required-evidence provenance
### Documentation

- Clarify token-bounded code retrieval
### Features

- **mcp:** Align discovery and model-visible result contracts (#466) ([#466](https://github.com/morluto/leantoken/pull/466))
- Prioritize production file matches
- Guide truncated read budgets
- Add compact search projections
- Plan exhaustive identifier searches
- Separate task savings from retrieval compression
- Prioritize primary context owners
### Performance

- Attribute no-op reconciliation writes
### Refactoring

- Make invalid application states unrepresentable
- **test:** Remove redundant fixture framework
### Styling

- Format rebased retrieval changes
- Format rebased test assertions
- Format integration test modules
### Testing

- Preserve sealed fixture contracts
- Validate structural benchmark manifests
- Cover published policy and npm versioning
- Bind published benchmark fixtures
- Preserve report artifact bindings
- Verify published evidence bindings
- Remove implementation-coupled checks
- Eliminate false-green fixture assumptions
- Bind checked-in fixture evidence

## [0.1.24] - 2026-08-06
### Bug Fixes

- **mcp:** Continue checking remaining contexts on receipt read error
- Critical bugs in staging, storage, ranking, MCP, and parser
- **ci:** Grant Cargo recovery OIDC permission
- **ci:** Identify crates.io availability probes
### Chores

- **ci:** Remove temporary Cargo recovery path [no-release]
- **ci:** Add versioned Cargo recovery path [no-release]
- **ci:** Add temporary Cargo 0.1.23 recovery workflow [no-release]

## [0.1.23] - 2026-08-05
### Bug Fixes

- **setup:** Migrate all generated legacy launchers
- **setup:** Preserve discovery on empty refresh
- **mcp:** Register contexts before primary startup

## [0.1.22] - 2026-08-04
### Bug Fixes

- **mcp:** Propagate CLI bounds to approved contexts
- **test:** Allow bounded MCP index readiness
- **services:** Enforce normalized inputs and configured limits
### Features

- **mcp:** Isolate approved repository contexts
### Refactoring

- Isolate index snapshots and decompose process tests (#436) ([#436](https://github.com/morluto/leantoken/pull/436))

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
