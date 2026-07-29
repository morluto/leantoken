#!/usr/bin/env python3
"""Run product tests with process-heavy tests isolated from CPU-heavy tests."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COMMON = ["cargo", "test", "--all-features"]


def run_parallel(lanes: list[tuple[str, list[str]]]) -> int:
    processes: list[tuple[str, subprocess.Popen[object]]] = []
    try:
        for name, command in lanes:
            print(f"==> {name}: {' '.join(command)}", flush=True)
            processes.append((name, subprocess.Popen(command, cwd=ROOT)))

        statuses = [(name, process.wait()) for name, process in processes]
    except KeyboardInterrupt:
        for _, process in processes:
            process.terminate()
        for _, process in processes:
            process.wait()
        return 130

    for name, status in statuses:
        if status:
            print(f"{name} tests failed with exit code {status}", file=sys.stderr)
    return 1 if any(status for _, status in statuses) else 0


def main() -> int:
    ordinary_status = run_parallel(
        [
            ("library and binary units", [*COMMON, "--lib", "--bins"]),
            (
                "ordinary integration",
                [*COMMON, "--test", "integration", "--", "--skip", "binary::"],
            ),
        ]
    )
    if ordinary_status:
        return ordinary_status

    return subprocess.run(
        [
            *COMMON,
            "--test",
            "integration",
            "binary::",
            "--",
            "--test-threads=2",
        ],
        cwd=ROOT,
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
