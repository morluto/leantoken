# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed

- Make `leantoken_context` the preferred first MCP call for broad repository tasks and position the narrow retrieval tools explicitly against shell equivalents.
- Replace ambiguous flat `leantoken_files` and `leantoken_read` arguments with tagged operation and target objects; the old MCP argument shapes are no longer accepted.
- Publish closed, bounded MCP input schemas with concrete defaults and examples.
- Return successful structured retry guidance while the index is starting, building, or changing instead of transient tool errors.

## [0.1.5] - 2026-07-17
### Documentation

- Document bundled npm distribution
