#!/usr/bin/env python3
"""Print release-planning outputs without changing the repository."""

import re
import subprocess
import sys
import tomllib
from pathlib import Path


def output(**values):
    for key, value in values.items():
        print(f"{key}={value}")


def main():
    latest = subprocess.run(
        ["git", "describe", "--tags", "--abbrev=0"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    if latest.returncode:
        print("::notice::No release tag found; bootstrap the release first", file=sys.stderr)
        output(skip="true")
        return

    latest_tag = latest.stdout.strip()
    latest_version = latest_tag.removeprefix("v")
    with Path("Cargo.toml").open("rb") as manifest:
        current_version = tomllib.load(manifest)["package"]["version"]
    output(latest_tag=latest_tag, latest_version=latest_version)
    if current_version != latest_version:
        print(
            f"::notice::Package version {current_version} is awaiting its matching "
            "release tag; skipping release planning",
            file=sys.stderr,
        )
        output(skip="true")
        return

    revision_range = f"{latest_tag}..HEAD"
    count = subprocess.check_output(
        ["git", "rev-list", "--count", revision_range], text=True
    ).strip()
    output(commits_since=count)
    if count == "0":
        output(skip="true")
        return

    # Use the same conventional-commit parser and policy as the changelog.
    next_tag = subprocess.check_output(
        [
            "git-cliff", "--offline", "--config", "cliff.toml",
            "--unreleased", "--bumped-version", revision_range,
        ],
        text=True,
    ).strip()
    version = next_tag.removeprefix("v")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError(f"expected a stable release version, got {next_tag!r}")
    output(new_version=version, new_tag=f"v{version}", skip="false")


if __name__ == "__main__":
    main()
