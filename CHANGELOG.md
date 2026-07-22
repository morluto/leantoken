# Changelog

All notable changes to this project will be documented in this file.
## [0.1.10] - 2026-07-22
### Bug Fixes

- Keep JSON error stderr parseable
- Close structured CLI error gaps
- Reject empty read symbols before sync
- Preflight request-only failures
- Validate requests before reconciliation
- Preserve configured limits through mcp startup
- Close request limit validation gaps
- Reject invalid request limits
- **search:** Bound pages and reuse literal matchers
- **watcher:** Bound recursive watch registration
- **tokens:** Truncate estimates in one pass
- **repository:** Stop changed-path parsing at limit
- **cache:** Exclude unsafe entries from size pruning
- Normalize file tree roots
- Canonicalize method symbol identities
- Keep search matches inside excerpts
- Preserve exact live read ranges
- Align benchmark reports and tests with diff-scoped context changes
- Resolve grouped Rust imports, bound watcher, cache parsers, optimize read/search/truncate
### Features

- Add structured CLI error categories
- Expose index readiness in status
- Expose diff scope in MCP schema and CLI flags
- Wire diff scope validation and ranking seed into context
- Add git_diff_paths resolver for base-revision diff scope
- Add DiffScope model for diff-scoped context retrieval
### Testing

- Use platform CLI name in clap assertions
- Add diff-scoped context integration tests

