from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate_agents_md.py")
SPEC = importlib.util.spec_from_file_location("validate_agents_md", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def write_fixture(root: Path) -> None:
    (root / ".cargo").mkdir()
    for relative in MODULE.REQUIRED_PATHS:
        (root / relative).mkdir(exist_ok=True)
    (root / "AGENTS.md").write_text(
        "# Repository guidance\n"
        + ("Keep deterministic behavior.\n" * 5)
        + "Use `cargo test-focused module::` and `cargo test-extras`.\n"
        + "Run `cargo test-contract` when retrieval accounting changes.\n"
        + "\n".join(sorted(MODULE.EXPECTED_GATE_COMMANDS))
        + "\n",
        encoding="utf-8",
    )
    alias_lines = ["[alias]"]
    alias_lines.extend(
        f'{name} = "{command}"'
        for name, command in MODULE.EXPECTED_ALIASES.items()
    )
    (root / ".cargo" / "config.toml").write_text(
        "\n".join(alias_lines) + "\n",
        encoding="utf-8",
    )


class ValidateAgentsMdTests(unittest.TestCase):
    def test_valid_fixture_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_fixture(root)
            commands = {
                "fmt",
                "clippy",
                "test-focused",
                "test-product",
                "test-contract",
                "test-extras",
            }

            self.assertEqual(MODULE.validate(root, commands), [])

    def test_unknown_command_and_gate_drift_fail(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_fixture(root)
            agents = (root / "AGENTS.md").read_text(encoding="utf-8")
            agents = agents.replace(
                "cargo test-product",
                "cargo test-produt",
            )
            (root / "AGENTS.md").write_text(agents, encoding="utf-8")

            commands = {
                "fmt",
                "clippy",
                *MODULE.EXPECTED_ALIASES,
            }
            errors = MODULE.validate(root, commands)

            self.assertTrue(
                any("gate commands differ" in error for error in errors),
                errors,
            )
            self.assertTrue(
                any("test-produt" in error for error in errors),
                errors,
            )

    def test_alias_drift_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_fixture(root)
            config = (root / ".cargo" / "config.toml").read_text(
                encoding="utf-8"
            )
            config = config.replace(
                MODULE.EXPECTED_ALIASES["test-product"],
                "test --all-features",
            )
            (root / ".cargo" / "config.toml").write_text(
                config,
                encoding="utf-8",
            )
            commands = {
                "fmt",
                "clippy",
                "test-focused",
                "test-product",
                "test-contract",
                "test-extras",
            }

            errors = MODULE.validate(root, commands)

            self.assertTrue(
                any("test-product" in error for error in errors),
                errors,
            )

    def test_cli_does_not_create_cargo_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            environment = os.environ.copy()
            environment["CARGO_TARGET_DIR"] = str(target)

            result = subprocess.run(
                [sys.executable, str(SCRIPT)],
                cwd=SCRIPT.parent.parent,
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(target.exists())


if __name__ == "__main__":
    unittest.main()
