from __future__ import annotations

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("report_target_footprint.py")
SPEC = importlib.util.spec_from_file_location("report_target_footprint", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class ReportTargetFootprintTests(unittest.TestCase):
    def test_reports_exclusive_buckets_and_stale_incremental_generations(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            files = {
                "debug/incremental/old-generation/state.bin": b"old",
                "debug/incremental/current-generation/state.bin": b"current",
                "debug/deps/library.rlib": b"dependency",
                "debug/examples/profile": b"example",
                "debug/build/output": b"build",
                "debug/leantoken": b"binary",
                "release/leantoken": b"release",
                "package/archive.crate": b"package",
            }
            for relative, contents in files.items():
                path = target / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(contents)
            now = 2_000_000_000.0
            old = now - 30 * 86_400
            old_root = target / "debug/incremental/old-generation"
            for path in [old_root / "state.bin", old_root]:
                os.utime(path, (old, old))
            current_root = target / "debug/incremental/current-generation"
            for path in [current_root / "state.bin", current_root]:
                os.utime(path, (now, now))

            report = MODULE.scan_target(target, stale_days=14, now=now)

            self.assertTrue(report["exists"])
            self.assertEqual(report["incremental_generations"], 2)
            self.assertEqual(report["stale_incremental_generations"], 1)
            for bucket in MODULE.BUCKET_NAMES:
                self.assertIn(bucket, report["buckets"])
            expected_logical = sum(map(len, files.values()))
            self.assertEqual(report["logical_bytes"], expected_logical)
            self.assertEqual(
                sum(
                    bucket["logical_bytes"]
                    for bucket in report["buckets"].values()
                ),
                expected_logical,
            )

    def test_does_not_follow_target_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            target.mkdir()
            outside = root / "outside.bin"
            outside.write_bytes(b"x" * 1_000_000)
            link = target / "outside-link"
            try:
                link.symlink_to(outside)
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")

            report = MODULE.scan_target(target)

            self.assertEqual(report["symlinks_scanned"], 1)
            self.assertLess(report["logical_bytes"], outside.stat().st_size)

    def test_counts_regular_file_hard_links_once(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            original = target / "debug/deps/library.rlib"
            duplicate = target / "release/library.rlib"
            original.parent.mkdir(parents=True)
            duplicate.parent.mkdir(parents=True)
            original.write_bytes(b"shared artifact")
            try:
                os.link(original, duplicate)
            except OSError as error:
                self.skipTest(f"hard-link creation unavailable: {error}")

            report = MODULE.scan_target(target)

            self.assertEqual(report["logical_bytes"], len(b"shared artifact"))
            self.assertEqual(
                sum(
                    bucket["logical_bytes"]
                    for bucket in report["buckets"].values()
                ),
                len(b"shared artifact"),
            )

    def test_missing_target_is_a_zero_footprint(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            report = MODULE.scan_target(Path(directory) / "missing")

            self.assertFalse(report["exists"])
            self.assertEqual(report["allocated_bytes"], 0)
            self.assertEqual(report["entries_scanned"], 0)

    def test_scan_fails_closed_at_the_entry_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            (target / "one").write_text("1", encoding="utf-8")
            (target / "two").write_text("2", encoding="utf-8")

            with self.assertRaisesRegex(
                MODULE.FootprintError,
                "exceeded max-entries=1",
            ):
                MODULE.scan_target(target, max_entries=1)


if __name__ == "__main__":
    unittest.main()
