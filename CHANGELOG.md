# Changelog

All notable changes to this project will be documented in this file.
## [0.1.1] - 2026-07-16
### Benchmarks

- Publish useful-line recall ablation
- Publish pinned Tokio indexing profile
- Profile pinned real repositories safely
- Harden model experiment reporting
- Publish sealed holdout result
- Seal unseen retrieval holdout
- Validate blind holdout coverage
- Publish synthetic MCP wire costs
- Generate deterministic MCP wire fixture
- Add retrieval and model evaluation harnesses
- Freeze prospective validation tasks
### Bug Fixes

- Classify configuration and capability errors
- Make indexing cancellation interrupt parsing
- Synchronize package version in lockfile
- Initialize index worker pools lazily
- Guard against null string in release-please step outputs
- Default release-please PR outputs to empty strings instead of null
- Request-scoped snapshots, content-hash reconcile, and hot-path bounds
- Coordinate MCP startup indexing
- Honor configured home during MCP setup
### Chores

- **main:** Release leantoken 0.1.1
- **main:** Release leantoken 0.1.1
### Continuous Integration

- Allow recorded benchmark revision
- Switch to release-please PR workflow, remove Dependabot, document release policy
- Add automated release pipeline, changelog generation, and dependency age policy
### Documentation

- Keep retrieval contracts synchronized
- Document measurement limits and workflows
### Features

- Add automatic LeanToken updates
- Preserve early task terms in context queries
- Improve retrieval efficiency and resilience
### Refactoring

- Propagate profile setup failures
- Type adapter failure outcomes
- Distinguish inapplicable candidate checks
- Remove unused storage publication paths
- Propagate fixture serialization errors
- Colocate retrieval entrypoints
- Bound retrieval paths and split services

