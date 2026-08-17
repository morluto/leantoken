# Release Process

LeanToken uses a release-please style workflow.
Every push to `main` triggers the [Release Please] workflow, which opens or
updates a release PR containing the version bump and regenerated changelog.
Merging that PR creates the git tag and explicitly dispatches [cargo-dist] with
that tag to publish platform archives to a GitHub release. Tag creation is
pinned to the release PR's merged commit from the close-event payload, so a
later `main` commit cannot be included in the earlier release. The tag step also
requires the PR title, Cargo package, and MCP Registry metadata to carry the
same version before it writes the tag. A custom packaging job then assembles
the five native binaries into one script-free npm tarball.
A trusted-publishing job verifies that tarball and publishes it to npm with
provenance before the release workflow completes.

The release workflow also publishes the Cargo package to crates.io with
`rust-lang/crates-io-auth-action`, then publishes the matching `server.json`
metadata to the official MCP Registry with `mcp-publisher`. Registry metadata
is not a second server artifact: it points clients at the exact Cargo version
that was just published.

The Cargo package, git tag, GitHub release, and npm package must use the same
exact version. MCP setup embeds `CARGO_PKG_VERSION` in npx launchers, so a
release with mismatched package metadata would create a launcher for a package
that does not exist. After publishing, users opt into that release with
`npx --yes leantoken@VERSION setup --refresh --yes`; existing entries never
advance automatically.

Before the first automated Cargo release, publish the initial version manually
with `cargo publish --locked`, then configure `morluto/leantoken` as a trusted
publisher for the `release.yml` GitHub Actions workflow on crates.io.
After that bootstrap, tagged releases publish Cargo first and MCP Registry
metadata second. The MCP Registry namespace is
`io.github.morluto/leantoken`.

## Release Cadence

- **On every push to `main`**: the [Release Please] workflow determines the
  next version (patch bump), generates the changelog with [git-cliff], bumps
  the version in `Cargo.toml`, and opens a PR labeled `autorelease`. Merging
  the PR creates the tag and dispatches the [Release] workflow, which builds the
  platform archives and single npm tarball from that exact tag.
- **Weekly scheduled run**: the [Release Please] workflow also runs every
  Monday at 09:00 UTC as a safety net to catch any unreleased changes.
- **Manual trigger**: the workflow supports `workflow_dispatch` for ad-hoc
  releases.

To suppress a release on a particular commit, include `[no-release]` or
`[skip ci]` in the commit message.

GitHub does not start a new workflow from a tag pushed by a workflow's
[`GITHUB_TOKEN`], so Release Please explicitly uses `workflow_dispatch` after
creating the tag. If merging the release PR creates the tag but no [Release]
run, fetch the annotated tag, verify that its peeled commit is the release PR
merge, then dispatch the release manually:

```bash
git fetch origin tag vVERSION --force
git rev-parse 'vVERSION^{}'
gh workflow run release.yml --ref vVERSION --field tag=vVERSION
```

Do not recreate or move the tag during this recovery. The dispatched workflow
checks out the supplied tag rather than the current tip of `main`.

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
[`GITHUB_TOKEN`]: https://docs.github.com/en/actions/concepts/security/github_token#when-github_token-triggers-workflow-runs
