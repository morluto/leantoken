# Changelog

All notable changes to this project will be documented in this file.
## [0.1.13] - 2026-07-24
### Benchmarks

- Add agent evaluation fixtures and trajectory sanitizer
### Bug Fixes

- Use cross-platform capability directory
- Harden retrieval and adapter boundaries
- Ensure prepare batches advance
- Preserve empty target boundaries in git diff hunk parser
- Advance batch end past oversized single candidate
### Chores

- Remove agent evaluation fixtures
- Ignore local snapshot and codex design artifacts
### Continuous Integration

- Speed up Rust test workflows
### Documentation

- Note stale-install workaround for npx bootstrap
### Features

- Resolve qualified symbols and surface typed symbol_not_found
### Testing

- Avoid filesystem-dependent path fixture
- Separate frozen reports from runtime invariants

