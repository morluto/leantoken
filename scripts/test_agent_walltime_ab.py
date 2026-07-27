from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import threading
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("agent_walltime_ab.py")
SPEC = importlib.util.spec_from_file_location("agent_walltime_ab", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentWalltimeAbTests(unittest.TestCase):
    def test_mcp_process_drains_and_bounds_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = root / "noisy-mcp"
            server.write_text(
                """#!/usr/bin/env python3
import json
import sys

sys.stderr.write("diagnostic-line\\n" * 20_000)
sys.stderr.flush()
for line in sys.stdin:
    request = json.loads(line)
    if request.get("id") == 0:
        print(json.dumps({"jsonrpc": "2.0", "id": 0, "result": {}}), flush=True)
""",
                encoding="utf-8",
            )
            server.chmod(0o755)
            mcp = MODULE.McpProcess(server, root, root / "index.sqlite3")
            failures: list[BaseException] = []

            def initialize() -> None:
                try:
                    mcp.initialize()
                except BaseException as error:
                    failures.append(error)

            worker = threading.Thread(target=initialize)
            worker.start()
            worker.join(timeout=5)
            if worker.is_alive():
                mcp.close()
                worker.join(timeout=1)
                self.fail("MCP initialization deadlocked while stderr was noisy")
            mcp.close()

            if failures:
                raise failures[0]
            captured = mcp._captured_stderr()
            self.assertIn("diagnostic-line", captured)
            self.assertLessEqual(
                len(captured),
                MODULE.McpProcess.STDERR_CAPTURE_CHARS,
            )

    def test_mcp_process_surfaces_captured_stderr_on_unexpected_exit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server = root / "failing-mcp"
            server.write_text(
                """#!/usr/bin/env python3
import sys

sys.stdin.readline()
sys.stderr.write("fatal startup diagnostic\\n")
sys.stderr.flush()
""",
                encoding="utf-8",
            )
            server.chmod(0o755)
            mcp = MODULE.McpProcess(server, root, root / "index.sqlite3")

            try:
                with self.assertRaisesRegex(
                    MODULE.InvalidEvidence,
                    "fatal startup diagnostic",
                ):
                    mcp.initialize()
            finally:
                mcp.close()

    def test_percentile_uses_nearest_rank(self) -> None:
        values = [float(value) for value in range(1, 21)]

        self.assertEqual(MODULE.percentile(values, 0.50), 10.0)
        self.assertEqual(MODULE.percentile(values, 0.95), 19.0)

    def test_leantoken_occurrences_convert_global_bytes_to_line_columns(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source.rs").write_text(
                "alpha\nprefix target suffix\n", encoding="utf-8"
            )
            response = {
                "hits": [
                    {
                        "path": "source.rs",
                        "occurrence": {
                            "start_line": 2,
                            "end_line": 2,
                            "start_byte": 13,
                            "end_byte": 19,
                        },
                    }
                ],
                "occurrences_returned": 1,
                "occurrences_total": 1,
            }

            self.assertEqual(
                MODULE.parse_leantoken_occurrences(response, root),
                [MODULE.Occurrence("source.rs", 2, 7, 13)],
            )

    def test_exhaustive_occurrence_parser_rejects_truncation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source.rs").write_text("target\n", encoding="utf-8")
            response = {
                "hits": [
                    {
                        "path": "source.rs",
                        "occurrence": {
                            "start_line": 1,
                            "end_line": 1,
                            "start_byte": 0,
                            "end_byte": 6,
                        },
                    }
                ],
                "occurrences_returned": 1,
                "occurrences_total": 2,
            }

            with self.assertRaisesRegex(
                MODULE.InvalidEvidence, "did not return every occurrence"
            ):
                MODULE.parse_leantoken_occurrences(response, root)

    def test_grouped_leantoken_occurrences_preserve_every_coordinate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source.rs").write_text(
                "target target\n", encoding="utf-8"
            )
            response = {
                "groups": [
                    {
                        "path": "source.rs",
                        "start_line": 1,
                        "end_line": 1,
                        "occurrences": [
                            {
                                "line": 1,
                                "start_column": 0,
                                "end_column": 6,
                            },
                            {
                                "line": 1,
                                "start_column": 7,
                                "end_column": 13,
                            },
                        ],
                    }
                ],
                "occurrences_returned": 2,
                "occurrences_total": 2,
            }

            self.assertEqual(
                MODULE.parse_leantoken_occurrences(response, root),
                [
                    MODULE.Occurrence("source.rs", 1, 0, 6),
                    MODULE.Occurrence("source.rs", 1, 7, 13),
                ],
            )

    def test_measure_pair_counterbalances_order_and_keeps_raw_samples(self) -> None:
        native_calls = 0
        lean_calls = 0

        def native() -> tuple[str, int]:
            nonlocal native_calls
            native_calls += 1
            return "same", 10

        def leantoken() -> tuple[str, int]:
            nonlocal lean_calls
            lean_calls += 1
            return "same", 20

        samples, native_value, lean_value, native_bytes, lean_bytes = (
            MODULE.measure_pair(
                4,
                native,
                leantoken,
                lambda left, right: self.assertEqual(left, right),
            )
        )

        self.assertEqual(native_calls, 4)
        self.assertEqual(lean_calls, 4)
        self.assertEqual(native_value, "same")
        self.assertEqual(lean_value, "same")
        self.assertEqual(native_bytes, 10)
        self.assertEqual(lean_bytes, 20)
        self.assertEqual(
            [sample["order"] for sample in samples],
            [
                "native-leantoken",
                "leantoken-native",
                "native-leantoken",
                "leantoken-native",
            ],
        )

    def test_canonical_context_ignores_only_receipt_identity(self) -> None:
        first = {
            "fragments": [{"path": "src/lib.rs", "source": "one"}],
            "meta": {"receipt_id": "r1", "emitted_tokens": 4},
            "receipt": {"receipt_id": "r1", "fragment_hashes": ["abc"]},
        }
        second = json.loads(json.dumps(first))
        second["meta"]["receipt_id"] = "r2"
        second["receipt"]["receipt_id"] = "r2"

        self.assertEqual(
            MODULE.canonical_context(first), MODULE.canonical_context(second)
        )
        second["fragments"][0]["source"] = "two"
        self.assertNotEqual(
            MODULE.canonical_context(first), MODULE.canonical_context(second)
        )

    def test_workload_manifest_binds_validation_manifest(self) -> None:
        repository = SCRIPT.parent.parent
        manifest_path = repository / "benchmarks/agent_walltime_ab.json"
        validation_path = repository / "benchmarks/validation.json"
        manifest = MODULE.load_json(manifest_path)

        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(
            manifest["source_manifest_sha256"],
            MODULE.sha256_file(validation_path),
        )
        self.assertEqual(
            [item["name"] for item in manifest["corpora"]],
            [
                "flask-validation",
                "gin-validation",
                "express-validation",
                "tokio-validation",
            ],
        )

    def test_markdown_surfaces_quality_and_suite_latency(self) -> None:
        operation = {
            "sum_of_corpus_medians": {
                "native_ms": 10.0,
                "leantoken_ms": 15.0,
                "delta": {"absolute_ms": 5.0, "relative": 0.5, "ratio": 1.5},
            }
        }
        report = {
            "status": "passed_accuracy_gates",
            "provenance": {
                "source_revision": "a" * 40,
                "host_os": "linux",
                "host_arch": "x86_64",
            },
            "protocol": {"iterations": 30, "context_iterations": 10},
            "aggregate": {
                "quality": {
                    "native_discovery_relevant_files_found": 4,
                    "context_relevant_files_found": 3,
                    "relevant_files": 4,
                    "context_line_anchors_found": 6,
                    "context_line_anchors": 8,
                    "native_discovery_relevant_file_recall": 1.0,
                    "context_relevant_file_recall": 0.75,
                    "context_line_anchor_recall": 0.75,
                },
                "exact_search": operation,
                "exact_read": operation,
                "discovery_context": operation,
            },
            "corpora": [],
            "limitations": ["diagnostic only"],
        }

        markdown = MODULE.markdown_report(report)

        self.assertIn("Native discovery relevant-file recall: 4/4 (100.0%)", markdown)
        self.assertIn("Context relevant-file recall: 3/4 (75.0%)", markdown)
        self.assertIn("| Exhaustive exact search | 10.00 ms | 15.00 ms", markdown)


if __name__ == "__main__":
    unittest.main()
