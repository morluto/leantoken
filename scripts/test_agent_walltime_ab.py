from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("agent_walltime_ab.py")
SPEC = importlib.util.spec_from_file_location("agent_walltime_ab", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentWalltimeAbTests(unittest.TestCase):
    def start_readiness_peer(self, mode: str) -> tuple[object, Path]:
        workspace = tempfile.TemporaryDirectory()
        self.addCleanup(workspace.cleanup)
        root = Path(workspace.name)
        requests = root / "requests.jsonl"
        server = root / "readiness-mcp"
        server.write_text(
            f"#!{sys.executable}\n"
            + """import json
import sys
import time
from pathlib import Path

mode = MODE
requests = Path(REQUESTS)
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    with requests.open("a", encoding="utf-8") as output:
        output.write(json.dumps(request) + "\\n")
    request_id = request["id"]
    if request_id == 0:
        result = {}
    else:
        if mode == "silent":
            time.sleep(10)
            continue
        if mode == "partial":
            sys.stdout.write('{"jsonrpc":"2.0","id":')
            sys.stdout.flush()
            time.sleep(10)
            continue
        if mode == "delayed":
            time.sleep(0.2)
        if mode == "notification":
            print(json.dumps({"jsonrpc": "2.0", "method": "notifications/progress"}), flush=True)
        if mode == "flood":
            while True:
                print(json.dumps({"jsonrpc": "2.0", "method": "notifications/progress"}), flush=True)
        result = {"structuredContent": {"status": "retryable" if mode == "retryable" else "ready", "paths": []}}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    if request_id and mode == "delayed":
        requests.with_suffix(".reply").write_text("sent", encoding="utf-8")
""".replace(
                "MODE", repr(mode)
            ).replace(
                "REQUESTS", repr(str(requests))
            ),
            encoding="utf-8",
        )
        server.chmod(0o755)
        prior_threads = {thread.ident for thread in threading.enumerate()}
        mcp = MODULE.McpProcess(server, root, root / "index.sqlite3")

        def close() -> None:
            mcp.close()
            self.assertIsNotNone(mcp.process.poll())
            leaked = [
                thread.name
                for thread in threading.enumerate()
                if thread.ident not in prior_threads
                and thread.name.startswith("leantoken-benchmark-")
            ]
            self.assertEqual(leaked, [])

        self.addCleanup(close)
        mcp.initialize()
        return mcp, requests

    def readiness_outcome(self, mcp: object) -> tuple[object, threading.Thread]:
        results: list[object] = []

        def wait() -> None:
            try:
                results.append(mcp.wait_ready(timeout_seconds=0.025))
            except BaseException as error:
                results.append(error)

        worker = threading.Thread(target=wait, daemon=True)
        worker.start()
        worker.join(timeout=0.6)
        if worker.is_alive():
            mcp.close()
            worker.join(timeout=1)
            self.fail("readiness remained blocked past its deadline and watchdog")
        self.assertEqual(len(results), 1)
        return results[0], worker

    def test_mcp_readiness_deadline_covers_silent_and_partial_peers(self) -> None:
        for mode in ("silent", "partial"):
            with self.subTest(mode=mode):
                mcp, _ = self.start_readiness_peer(mode)
                result, _ = self.readiness_outcome(mcp)
                self.assertIsInstance(result, MODULE.InvalidEvidence)
                self.assertRegex(str(result), "deadline|timed out")
                mcp.close()
                self.assertIsNotNone(mcp.process.poll())

    def test_mcp_readiness_rejects_delayed_reply_and_prevents_reuse(self) -> None:
        mcp, requests = self.start_readiness_peer("delayed")
        result, _ = self.readiness_outcome(mcp)
        self.assertIsInstance(result, MODULE.InvalidEvidence)
        self.assertRegex(str(result), "deadline|timed out")
        reply_sent = requests.with_suffix(".reply")
        deadline = time.monotonic() + 1
        while not reply_sent.exists() and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertTrue(reply_sent.exists(), "peer did not send its late reply")
        with self.assertRaisesRegex(MODULE.InvalidEvidence, "unusable|closed"):
            mcp.call("files", {"operation": {"kind": "tree"}, "max_results": 1})
        mcp.close()
        sent = [json.loads(line) for line in requests.read_text().splitlines()]
        self.assertEqual([request["id"] for request in sent], [0, 1])

    def test_mcp_readiness_accepts_matching_reply_after_notification(self) -> None:
        mcp, _ = self.start_readiness_peer("notification")
        self.assertIsInstance(mcp.wait_ready(timeout_seconds=1), float)

    def test_mcp_readiness_budget_survives_retries_and_notifications(self) -> None:
        for mode in ("retryable", "flood"):
            with self.subTest(mode=mode):
                mcp, _ = self.start_readiness_peer(mode)
                result, _ = self.readiness_outcome(mcp)
                self.assertIsInstance(result, MODULE.InvalidEvidence)
                self.assertRegex(str(result), "deadline|timed out")

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
            (root / "source.rs").write_text("target target\n", encoding="utf-8")
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
            "meta": {"receipt_id": "r1", "source_tokens": 4},
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


if __name__ == "__main__":
    unittest.main()
