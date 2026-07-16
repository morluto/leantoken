# Release Process

LeanToken practices continuous deployment via a release-please style workflow.
Every push to `main` triggers the [Release Please] workflow, which opens or
updates a release PR containing the version bump and regenerated changelog.
Merging that PR creates the git tag, which triggers [cargo-dist] to build and
publish platform binaries.

## Release Cadence

- **On every push to `main`**: the [Release Please] workflow determines the
  next version (patch bump), generates the changelog with [git-cliff], bumps
  the version in `Cargo.toml`, and opens a PR labeled `autorelease`. Merging
  the PR creates the tag and triggers the [Release] workflow.
- **Weekly scheduled run**: the [Release Please] workflow also runs every
  Monday at 09:00 UTC as a safety net to catch any unreleased changes.
- **Manual trigger**: the workflow supports `workflow_dispatch` for ad-hoc
  releases.

To suppress a release on a particular commit, include `[no-release]` or
`[skip ci]` in the commit message.

## Changelog Generation

Release notes are generated automatically by [git-cliff] from conventional
commit messages and posted as the release PR body. The configuration in
[`cliff.toml`](../cliff.toml) maps commit types to changelog sections:

| Prefix     | Section                |
|------------|------------------------|
| `feat`     | Features               |
| `fix`      | Bug Fixes              |
| `perf`     | Performance            |
| `refactor` | Refactoring            |
| `docs`     | Documentation          |
| `test`     | Testing                |
| `ci`       | Continuous Integration |
| `chore`    | Chores                 |
| `bench`    | Benchmarks             |

## Dependency Update Policy

Dependencies are updated weekly via [Renovate]. New dependency releases must be
at least 3 days old (for Cargo crates) or 7 days old (for GitHub Actions)
before Renovate will open a PR. This delay mitigates supply chain risks by
allowing time for compromised releases to be detected and yanked before
adoption. See [`.github/renovate.json5`](../.github/renovate.json5) for the
full configuration.

[Release Please]: ../.github/workflows/release-please.yml
[Release]: ../.github/workflows/release.yml
[cargo-dist]: https://github.com/axodotdev/cargo-dist
[git-cliff]: https://git-cliff.org
[Renovate]: https://docs.renovatebot.com
