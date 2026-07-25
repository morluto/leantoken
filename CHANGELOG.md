# Changelog

All notable changes to this project will be documented in this file.
## [0.1.15] - 2026-07-25
### Benchmarks

- Add paired performance regression runner
### Bug Fixes

- Make truncated reads fail loud
- **doctor:** Await terminal child diagnostics
- Exclude nested Git metadata
### Features

- Add context query plans
- Report runtime footprint in status
- Add structural JSON retrieval
- Add symbol-aware git history
- Add context omission coverage diagnostics
- Clarify retrieval consistency names
- Add server-managed retrieval receipts
- Add search coverage diagnostics
- Exclude generated artifacts from context
- Report model-facing token costs
- **parser:** Add Markdown section navigation
- Add strict context scopes
- Report outline completeness
- Distinguish source tokens from payload tokens
- **parser:** Add HTML and CSS structural outlines
- **parser:** Index JavaScript and TypeScript top-level data bindings
- Expose exhaustive occurrence search
- Add context coverage contract
### Performance

- **tokens:** Avoid redundant BPE encoding during truncation
- **storage:** Deduplicate excerpt hydration
- **retrieval:** Reuse request-scoped hot-path work
### Testing

- **windows:** Initialize read continuation cursor

