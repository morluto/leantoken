# Changelog

All notable changes to this project will be documented in this file.
## [0.1.18] - 2026-07-28
### Benchmarks

- **indexing:** Record TileLang cold baseline
- **indexing:** Profile dependency-heavy cold builds
- Expand MCP multiprocess CPU profile
- **search:** Profile bounded regex fallback
- Profile persistent read delta reuse
- Add receipt persistence profile
- Seal frozen holdout vnext
### Bug Fixes

- Bound cold reconciliation waits
- Delay periodic watcher reconciliation
- **ci:** Preserve release workflow timeouts
- Reserve persistent receipt response cost
- Stabilize receipt semantic signatures
- Remove redundant context stage borrows
- Aggregate context option conflicts
- Report exact response budget retry minimum
- Fail closed on retrieval integrity errors
- **doctor:** Accept schema-qualified MCP versions
- **mcp:** Decouple rustdoc links from schema
- **context:** Reject empty focus and exclude inputs
- **search:** Match short text substrings
### Chores

- **mcp:** Return rmcp to upstream releases
### Continuous Integration

- Enforce bounded audit checks
- Account for include facade coverage
- Restore coverage gate to the measured baseline
### Documentation

- **mcp:** Prefer one-call autonomous triage (#349) ([#349](https://github.com/morluto/leantoken/pull/349))
- **readme:** Add Chinese Japanese and Korean translations
- Record read delta persistence evidence
- Bind receipt profile to budget fix
- Refresh receipt persistence profile
- Record receipt persistence profile
- Update service test ownership map
### Features

- Expose bounded cold index progress
- Persist safe read delta bases
- Persist retrieval receipts
- Negotiate exact MCP result modes
- Add context response profiles
- Add retrieval promotion gate
### Performance

- Cut retrieval hot-path accounting and scan work
### Refactoring

- Type read delta evaluation context
- Split search execution owners
- Split read execution owners
- Stage search execution pipeline
- Stage ranking selection pipeline
- Centralize public error categories
- Split binary lifecycle orchestration
- Split CLI command groups
- Separate watcher scheduling and runtime
- Separate cache policy and rendering
- Split ranking pipeline owners
- Split storage read and write owners
- Split index reconciliation owners
- Split setup transaction owners
- Split repository and Git owners
- Split parser language owners
- Split service lifecycle owners
- Converge retrieval execution paths
- Stage context retrieval pipeline
- Split public models by domain
- Separate MCP runtime responsibilities
- Split MCP request schemas by tool
- Split service integration test owners
- Clarify MCP and context boundaries
### Styling

- Normalize service owner file endings
- Normalize repository test module
- Normalize parser test module
### Testing

- Stabilize reconciliation cleanup assertions
- **storage:** Cover WAL checkpoint recovery
- Harden git-dependent coverage
- Update legacy schema fixtures
- Verify read delta cache ownership
- Canonicalize registry evidence lines
- Follow context signal owner split
- **context:** Follow compact omission contract
- **mcp:** Validate savings snapshot documentation
- Repair cross-platform CI fixtures
