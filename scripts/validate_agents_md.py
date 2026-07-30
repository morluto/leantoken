#!/usr/bin/env python3
"""Validate repository commands in AGENTS.md without executing an alias."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tomllib
from collections.abc import Collection
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_GATE_COMMANDS = {
    "cargo fmt --all -- --check",
    "cargo clippy --all-targets --all-features -- -D warnings",
    "cargo test-product",
}
EXPECTED_ALIASES = {
    "test-focused": "run --locked --package leantoken-xtask -- test-focused",
    "test-product": "run --locked --package leantoken-xtask -- test product",
    "test-contract": "run --locked --package leantoken --example benchmark-contract",
    "test-extras": "test --locked --package leantoken --all-features --examples",
}
REQUIRED_PATHS = (
    "src",
    "tests",
    "benchmarks",
    "docs",
    "scripts",
    ".github",
)
CARGO_REFERENCE = re.compile(r"\bcargo\s+([A-Za-z0-9_-]+)")
CARGO_LIST_ENTRY = re.compile(r"^\s{4}(\S+)\s")


def available_cargo_commands(root: Path) -> tuple[set[str], str | None]:
    environment = os.environ.copy()
    environment["CARGO_TERM_COLOR"] = "never"
    result = subprocess.run(
        ["cargo", "--list"],
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    )
    if result.returncode:
        detail = result.stderr.strip() or f"exit code {result.returncode}"
        return set(), f"`cargo --list` failed: {detail}"
    return {
        match.group(1)
        for line in result.stdout.splitlines()
        if (match := CARGO_LIST_ENTRY.match(line))
    }, None


def validate(root: Path, cargo_commands: Collection[str]) -> list[str]:
    errors: list[str] = []
    agents_path = root / "AGENTS.md"
    if not agents_path.is_file():
        return ["AGENTS.md not found at repository root"]

    agents = agents_path.read_text(encoding="utf-8")
    if len(agents.encode("utf-8")) < 100:
        errors.append("AGENTS.md is too short (< 100 bytes)")

    documented_gates = {
        line.strip() for line in agents.splitlines() if line.startswith("cargo ")
    }
    if documented_gates != EXPECTED_GATE_COMMANDS:
        errors.append(
            "AGENTS.md gate commands differ from the canonical set: "
            f"expected {sorted(EXPECTED_GATE_COMMANDS)!r}, "
            f"found {sorted(documented_gates)!r}"
        )

    referenced_commands = set(CARGO_REFERENCE.findall(agents))
    unknown_commands = sorted(referenced_commands.difference(cargo_commands))
    if unknown_commands:
        errors.append(
            "AGENTS.md references unknown Cargo command(s): "
            + ", ".join(unknown_commands)
        )

    config_path = root / ".cargo" / "config.toml"
    if not config_path.is_file():
        errors.append(".cargo/config.toml not found")
    else:
        with config_path.open("rb") as config_file:
            aliases = tomllib.load(config_file).get("alias", {})
        for name, expected in EXPECTED_ALIASES.items():
            actual = aliases.get(name)
            if actual != expected:
                errors.append(
                    f"Cargo alias {name!r} differs from the canonical command: "
                    f"expected {expected!r}, found {actual!r}"
                )

    for relative in REQUIRED_PATHS:
        if not (root / relative).exists():
            errors.append(f"AGENTS.md references missing path {relative!r}")

    return errors


def main() -> int:
    print("==> Validating AGENTS.md commands...", flush=True)
    cargo_commands, cargo_error = available_cargo_commands(ROOT)
    errors = [cargo_error] if cargo_error else validate(ROOT, cargo_commands)
    for error in errors:
        print(f"::error::{error}", file=sys.stderr)
    if errors:
        print(
            f"::error::AGENTS.md validation found {len(errors)} issue(s)",
            file=sys.stderr,
        )
        return 1
    print("AGENTS.md validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
