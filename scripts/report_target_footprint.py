#!/usr/bin/env python3
"""Report bounded, read-only Cargo target-directory disk usage."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path


SCHEMA_VERSION = 1
DEFAULT_MAX_ENTRIES = 1_000_000
MAX_SCAN_DEPTH = 64
BUCKET_NAMES = (
    "debug_incremental",
    "debug_deps",
    "debug_examples",
    "debug_build",
    "debug_other",
    "release",
    "other",
)


class FootprintError(RuntimeError):
    """Raised when a bounded or consistent scan cannot be completed."""


def classify(relative: Path) -> str:
    parts = relative.parts
    if parts[:2] == ("debug", "incremental"):
        return "debug_incremental"
    if parts[:2] == ("debug", "deps"):
        return "debug_deps"
    if parts[:2] == ("debug", "examples"):
        return "debug_examples"
    if parts[:2] == ("debug", "build"):
        return "debug_build"
    if parts[:1] == ("debug",):
        return "debug_other"
    if parts[:1] == ("release",):
        return "release"
    return "other"


def allocated_size(stat_result: os.stat_result) -> tuple[int, bool]:
    blocks = getattr(stat_result, "st_blocks", None)
    if blocks is None:
        return stat_result.st_size, False
    return blocks * 512, True


def empty_report(target: Path, stale_days: int, max_entries: int) -> dict:
    return {
        "schema": SCHEMA_VERSION,
        "target_directory": str(target),
        "exists": False,
        "stale_after_days": stale_days,
        "max_entries": max_entries,
        "entries_scanned": 0,
        "directories_scanned": 0,
        "symlinks_scanned": 0,
        "logical_bytes": 0,
        "allocated_bytes": 0,
        "allocated_bytes_exact": True,
        "incremental_generations": 0,
        "stale_incremental_generations": 0,
        "buckets": {
            name: {"logical_bytes": 0, "allocated_bytes": 0}
            for name in BUCKET_NAMES
        },
    }


def scan_target(
    target: Path,
    *,
    stale_days: int = 14,
    max_entries: int = DEFAULT_MAX_ENTRIES,
    now: float | None = None,
) -> dict:
    if stale_days < 1:
        raise FootprintError("stale-days must be at least 1")
    if max_entries < 1:
        raise FootprintError("max-entries must be at least 1")

    target = target.resolve()
    report = empty_report(target, stale_days, max_entries)
    if not target.exists():
        return report
    if not target.is_dir():
        raise FootprintError(f"target path is not a directory: {target}")

    report["exists"] = True
    buckets = report["buckets"]
    stack = [(target, 0)]
    hard_links: dict[tuple[int, int], tuple[str, str, int, int]] = {}
    incremental_activity: dict[str, float] = {}
    allocated_exact = True

    try:
        root_stat = target.stat()
    except OSError as error:
        raise FootprintError(f"cannot stat target directory {target}: {error}") from error
    root_allocated, root_exact = allocated_size(root_stat)
    report["allocated_bytes"] += root_allocated
    buckets["other"]["allocated_bytes"] += root_allocated
    allocated_exact &= root_exact

    while stack:
        directory, depth = stack.pop()
        if depth >= MAX_SCAN_DEPTH:
            raise FootprintError(
                f"target scan exceeded maximum depth {MAX_SCAN_DEPTH}: {directory}"
            )
        try:
            entries = os.scandir(directory)
        except OSError as error:
            raise FootprintError(f"cannot read target directory {directory}: {error}") from error
        try:
            for entry in entries:
                report["entries_scanned"] += 1
                if report["entries_scanned"] > max_entries:
                    raise FootprintError(
                        f"target scan exceeded max-entries={max_entries}; "
                        "increase the explicit bound to inspect this directory"
                    )
                path = Path(entry.path)
                try:
                    stat_result = entry.stat(follow_symlinks=False)
                except OSError as error:
                    raise FootprintError(
                        f"target changed or became unreadable while scanning {path}: {error}"
                    ) from error
                relative = path.relative_to(target)
                bucket_name = classify(relative)
                bucket = buckets[bucket_name]
                is_directory = entry.is_dir(follow_symlinks=False)
                is_symlink = entry.is_symlink()
                if is_directory:
                    report["directories_scanned"] += 1
                if is_symlink:
                    report["symlinks_scanned"] += 1

                allocated, exact = allocated_size(stat_result)
                allocated_exact &= exact
                logical = 0 if is_directory else stat_result.st_size
                identity = (stat_result.st_dev, stat_result.st_ino)
                is_hard_link = (
                    not is_directory
                    and not is_symlink
                    and identity[1] != 0
                    and stat_result.st_nlink > 1
                )
                if not is_hard_link:
                    report["allocated_bytes"] += allocated
                    report["logical_bytes"] += logical
                    bucket["allocated_bytes"] += allocated
                    bucket["logical_bytes"] += logical
                else:
                    relative_name = relative.as_posix()
                    previous = hard_links.get(identity)
                    if previous is None:
                        hard_links[identity] = (
                            relative_name,
                            bucket_name,
                            logical,
                            allocated,
                        )
                        report["allocated_bytes"] += allocated
                        report["logical_bytes"] += logical
                        bucket["allocated_bytes"] += allocated
                        bucket["logical_bytes"] += logical
                    elif relative_name < previous[0]:
                        previous_bucket = buckets[previous[1]]
                        previous_bucket["logical_bytes"] -= previous[2]
                        previous_bucket["allocated_bytes"] -= previous[3]
                        bucket["logical_bytes"] += logical
                        bucket["allocated_bytes"] += allocated
                        hard_links[identity] = (
                            relative_name,
                            bucket_name,
                            logical,
                            allocated,
                        )

                parts = relative.parts
                if len(parts) >= 3 and parts[:2] == ("debug", "incremental"):
                    generation = parts[2]
                    incremental_activity[generation] = max(
                        incremental_activity.get(generation, 0.0),
                        stat_result.st_mtime,
                    )
                if is_directory:
                    stack.append((path, depth + 1))
        except OSError as error:
            raise FootprintError(
                f"target changed or became unreadable while scanning {directory}: {error}"
            ) from error
        finally:
            entries.close()

    cutoff = (time.time() if now is None else now) - stale_days * 86_400
    report["allocated_bytes_exact"] = allocated_exact
    report["incremental_generations"] = len(incremental_activity)
    report["stale_incremental_generations"] = sum(
        modified < cutoff for modified in incremental_activity.values()
    )
    return report


def format_bytes(value: int) -> str:
    amount = float(value)
    units = ("B", "KiB", "MiB", "GiB", "TiB")
    for unit in units:
        if amount < 1024 or unit == units[-1]:
            return f"{amount:.1f} {unit}"
        amount /= 1024
    raise AssertionError("unreachable")


def print_human(report: dict) -> None:
    print(f"Cargo target: {report['target_directory']}")
    if not report["exists"]:
        print("Target directory does not exist; footprint is 0 B.")
        return
    allocation_label = "allocated" if report["allocated_bytes_exact"] else "estimated allocated"
    print(
        f"Total: {format_bytes(report['allocated_bytes'])} {allocation_label}; "
        f"{format_bytes(report['logical_bytes'])} logical"
    )
    print(
        f"Scanned: {report['entries_scanned']:,} entries / "
        f"{report['directories_scanned']:,} directories / "
        f"{report['symlinks_scanned']:,} symlinks "
        f"(bound {report['max_entries']:,})"
    )
    print(
        "Incremental generations: "
        f"{report['incremental_generations']:,}; "
        f"{report['stale_incremental_generations']:,} inactive for more than "
        f"{report['stale_after_days']} days"
    )
    for name, values in report["buckets"].items():
        if values["allocated_bytes"] or values["logical_bytes"]:
            print(
                f"  {name}: {format_bytes(values['allocated_bytes'])} allocated; "
                f"{format_bytes(values['logical_bytes'])} logical"
            )
    print("Read-only report: no artifacts were removed.")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target-dir",
        type=Path,
        default=Path(os.environ.get("CARGO_TARGET_DIR", "target")),
    )
    parser.add_argument("--stale-days", type=int, default=14)
    parser.add_argument("--max-entries", type=int, default=DEFAULT_MAX_ENTRIES)
    parser.add_argument("--json", action="store_true", dest="as_json")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        report = scan_target(
            args.target_dir,
            stale_days=args.stale_days,
            max_entries=args.max_entries,
        )
    except FootprintError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    if args.as_json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print_human(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
