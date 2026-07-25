from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("paired_performance.py")


def write_collect_fixture(root: Path) -> tuple[Path, Path]:
    manifest = root / "manifest.json"
    samples = root / "samples"
    manifest.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "rustc_version_prefix": "rustc 1.95.0 ",
                "benchstat_version": "v1.2.3",
                "reports": {
                    "hot_path": {
                        "file": "hot-path.json",
                        "required": {"/release_build": True},
                        "parity": ["/fixture", "/response_blake3"],
                    }
                },
                "metrics": [
                    {
                        "benchmark": "HotPathRegexP50",
                        "report": "hot_path",
                        "pointer": "/regex/timing_ms/p50",
                        "source_unit": "ms",
                        "max_regression_percent": 10.0,
                        "min_absolute_regression_ns": 500_000,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    for pair in (1, 2):
        order = "AB" if pair == 1 else "BA"
        sequences = {
            "base": 1 if order == "AB" else 2,
            "head": 2 if order == "AB" else 1,
        }
        for side, latency in (("base", 2.0 + pair / 10), ("head", 1.0 + pair / 10)):
            sample = samples / f"{side}-{pair:02d}"
            sample.mkdir(parents=True)
            (sample / "provenance.json").write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "side": side,
                        "pair": pair,
                        "order": order,
                        "sequence": sequences[side],
                        "source_sha": ("a" if side == "base" else "b") * 40,
                        "source_tree_sha": ("c" if side == "base" else "d") * 40,
                        "source_dirty": False,
                        "rustc_version": "rustc 1.95.0 (test)",
                        "benchstat_version": "v1.2.3",
                        "host_os": "linux",
                        "host_arch": "x86_64",
                    }
                ),
                encoding="utf-8",
            )
            (sample / "hot-path.json").write_text(
                json.dumps(
                    {
                        "release_build": True,
                        "fixture": {"files": 100},
                        "response_blake3": {"regex": "f" * 64},
                        "regex": {"timing_ms": {"p50": latency}},
                    }
                ),
                encoding="utf-8",
            )
    return manifest, samples


def collect_command(
    manifest: Path, samples: Path, root: Path
) -> tuple[subprocess.CompletedProcess[str], Path, Path, Path]:
    base = root / "base.txt"
    head = root / "head.txt"
    parity = root / "parity.json"
    completed = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "collect",
            "--manifest",
            str(manifest),
            "--samples",
            str(samples),
            "--pairs",
            "2",
            "--base-out",
            str(base),
            "--head-out",
            str(head),
            "--parity-out",
            str(parity),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    return completed, base, head, parity


def gate_command(
    manifest: Path, benchstat_csv: Path, root: Path
) -> tuple[subprocess.CompletedProcess[str], Path, Path]:
    markdown = root / "report.md"
    report_json = root / "report.json"
    completed = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "gate",
            "--manifest",
            str(manifest),
            "--benchstat-csv",
            str(benchstat_csv),
            "--markdown-out",
            str(markdown),
            "--json-out",
            str(report_json),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    return completed, markdown, report_json


def write_benchstat_csv(root: Path, *, head_seconds: float, comparison: str) -> Path:
    benchstat_csv = root / "benchstat.csv"
    benchstat_csv.write_text(
        "\n".join(
            [
                "goos: linux",
                "goarch: amd64",
                "pkg: leantoken/paired-performance",
                ",base,,head,,,",
                ",sec/op,CI,sec/op,CI,vs base,P",
                f"HotPathRegexP50,0.002,1%,{head_seconds},1%,{comparison},p=0.000 n=10",
                f"geomean,0.002,,{head_seconds},,{comparison},",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    return benchstat_csv


class PairedPerformanceCliTests(unittest.TestCase):
    def test_collect_emits_counterbalanced_benchstat_inputs_and_parity_receipt(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            completed, base, head, parity = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("BenchmarkHotPathRegexP50 1 2100000 ns/op", base.read_text())
            self.assertIn("BenchmarkHotPathRegexP50 1 1200000 ns/op", head.read_text())
            receipt = json.loads(parity.read_text())
            self.assertEqual(receipt["pairs"], 2)
            self.assertEqual(receipt["parity"], "pass")
            self.assertEqual(receipt["base_source_sha"], "a" * 40)
            self.assertEqual(receipt["head_source_sha"], "b" * 40)

    def test_collect_rejects_observable_response_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            changed = samples / "head-02" / "hot-path.json"
            report = json.loads(changed.read_text())
            report["response_blake3"]["regex"] = "0" * 64
            changed.write_text(json.dumps(report), encoding="utf-8")

            completed, _, _, _ = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn("observable parity mismatch", completed.stderr)

    def test_collect_rejects_incorrect_counterbalance_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            changed = samples / "head-02" / "provenance.json"
            provenance = json.loads(changed.read_text())
            provenance["order"] = "AB"
            changed.write_text(json.dumps(provenance), encoding="utf-8")

            completed, _, _, _ = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn("order='AB', expected 'BA'", completed.stderr)

    def test_collect_rejects_unpinned_benchstat_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            changed = samples / "head-02" / "provenance.json"
            provenance = json.loads(changed.read_text())
            provenance["benchstat_version"] = "v9.9.9"
            changed.write_text(json.dumps(provenance), encoding="utf-8")

            completed, _, _, _ = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn(
                "benchstat_version='v9.9.9', expected 'v1.2.3'", completed.stderr
            )

    def test_collect_rejects_identical_base_and_head_commits(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            for pair in (1, 2):
                changed = samples / f"head-{pair:02d}" / "provenance.json"
                provenance = json.loads(changed.read_text())
                provenance["source_sha"] = "a" * 40
                changed.write_text(json.dumps(provenance), encoding="utf-8")

            completed, _, _, _ = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn(
                "base and head source commits are identical", completed.stderr
            )

    def test_collect_rejects_a_non_finite_metric(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, samples = write_collect_fixture(root)
            changed = samples / "head-02" / "hot-path.json"
            report = json.loads(changed.read_text())
            report["regex"]["timing_ms"]["p50"] = float("nan")
            changed.write_text(json.dumps(report), encoding="utf-8")

            completed, _, _, _ = collect_command(manifest, samples, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn("metric must be finite", completed.stderr)

    def test_gate_rejects_a_statistically_significant_material_regression(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _ = write_collect_fixture(root)
            benchstat_csv = write_benchstat_csv(
                root, head_seconds=0.003, comparison="+50.00%"
            )
            completed, markdown, report_json = gate_command(
                manifest, benchstat_csv, root
            )

            self.assertEqual(completed.returncode, 1, completed.stderr)
            self.assertIn("| `HotPathRegexP50` |", markdown.read_text())
            self.assertIn("| FAIL |", markdown.read_text())
            report = json.loads(report_json.read_text())
            self.assertEqual(report["decision"], "fail")
            self.assertEqual(report["failures"], ["HotPathRegexP50"])
            self.assertEqual(len(report["manifest_sha256"]), 64)
            self.assertEqual(len(report["benchstat_csv_sha256"]), 64)

    def test_gate_treats_a_small_absolute_change_as_noise(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _ = write_collect_fixture(root)
            benchstat_csv = write_benchstat_csv(
                root, head_seconds=0.0024, comparison="+20.00%"
            )

            completed, _, report_json = gate_command(manifest, benchstat_csv, root)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads(report_json.read_text())
            self.assertEqual(report["decision"], "pass")
            self.assertEqual(report["rows"][0]["status"], "NOISE")

    def test_gate_keeps_a_material_but_insignificant_change_inconclusive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _ = write_collect_fixture(root)
            benchstat_csv = write_benchstat_csv(
                root, head_seconds=0.003, comparison="~"
            )

            completed, _, report_json = gate_command(manifest, benchstat_csv, root)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads(report_json.read_text())
            self.assertEqual(report["decision"], "pass")
            self.assertEqual(report["rows"][0]["status"], "INCONCLUSIVE")

    def test_gate_rejects_a_missing_benchstat_significance_result(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _ = write_collect_fixture(root)
            benchstat_csv = write_benchstat_csv(root, head_seconds=0.003, comparison="")

            completed, _, _ = gate_command(manifest, benchstat_csv, root)

            self.assertEqual(completed.returncode, 2)
            self.assertIn("invalid Benchstat comparison", completed.stderr)


if __name__ == "__main__":
    unittest.main()
