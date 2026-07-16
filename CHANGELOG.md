# Changelog

All notable changes to this project will be documented in this file.
## [0.1.1] - 2026-07-16
### Benchmarks

- Add retrieval and model evaluation harnesses
- Freeze prospective validation tasks
### Bug Fixes

- Guard against null string in release-please step outputs
- Default release-please PR outputs to empty strings instead of null
- Request-scoped snapshots, content-hash reconcile, and hot-path bounds
- Coordinate MCP startup indexing
- Honor configured home during MCP setup
### Continuous Integration

- Switch to release-please PR workflow, remove Dependabot, document release policy
- Add automated release pipeline, changelog generation, and dependency age policy
### Documentation

- Document measurement limits and workflows
### Features

- Improve retrieval efficiency and resilience
### Refactoring

- Colocate retrieval entrypoints
- Bound retrieval paths and split services

