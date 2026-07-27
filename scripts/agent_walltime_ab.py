#!/usr/bin/env python3
"""Measure persistent LeanToken MCP and native repository-tool wall time."""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence


class InvalidEvidence(ValueError):
    """Raised when benchmark inputs or observable results are invalid."""


@dataclass(frozen=True, order=True)
class Occurrence:
    """One byte-exact source occurrence."""

    path: str
    line: int
    start_column_byte: int
    end_column_byte: int


class McpProcess:
    """Minimal newline-delimited JSON-RPC client for one LeanToken process."""

    STDERR_CAPTURE_CHARS = 64 * 1024

    def __init__(self, binary: Path, root: Path, database: Path) -> None:
        self.process = subprocess.Popen(
            [
                str(binary),
                "mcp",
                "--root",
                str(root),
                "--database",
                str(database),
                "--result-mode",
                "structured",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self._stderr_chunks: collections.deque[str] = collections.deque()
        self._stderr_chars = 0
        self._stderr_lock = threading.Lock()
        self._stderr_thread = threading.Thread(
            target=self._drain_stderr,
            name="leantoken-benchmark-stderr",
            daemon=True,
        )
        self._stderr_thread.start()
        self.next_id = 1

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self._stderr_thread.join(timeout=1)
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()

    def _drain_stderr(self) -> None:
        stream = self.process.stderr
        if stream is None:
            return
        while True:
            chunk = stream.read(4096)
            if not chunk:
                return
            with self._stderr_lock:
                if len(chunk) >= self.STDERR_CAPTURE_CHARS:
                    self._stderr_chunks.clear()
                    self._stderr_chunks.append(
                        chunk[-self.STDERR_CAPTURE_CHARS :]
                    )
                    self._stderr_chars = self.STDERR_CAPTURE_CHARS
                    continue
                self._stderr_chunks.append(chunk)
                self._stderr_chars += len(chunk)
                while self._stderr_chars > self.STDERR_CAPTURE_CHARS:
                    overflow = self._stderr_chars - self.STDERR_CAPTURE_CHARS
                    oldest = self._stderr_chunks[0]
                    if len(oldest) <= overflow:
                        self._stderr_chunks.popleft()
                        self._stderr_chars -= len(oldest)
                    else:
                        self._stderr_chunks[0] = oldest[overflow:]
                        self._stderr_chars -= overflow

    def _captured_stderr(self) -> str:
        with self._stderr_lock:
            return "".join(self._stderr_chunks)

    def _send(self, value: dict[str, Any]) -> None:
        if self.process.stdin is None:
            raise InvalidEvidence("MCP stdin is unavailable")
        self.process.stdin.write(
            json.dumps(value, separators=(",", ":"), ensure_ascii=False) + "\n"
        )
        self.process.stdin.flush()

    def _response(self, request_id: int) -> dict[str, Any]:
        if self.process.stdout is None:
            raise InvalidEvidence("MCP stdout is unavailable")
        while True:
            line = self.process.stdout.readline()
            if not line:
                self._stderr_thread.join(timeout=0.1)
                stderr = self._captured_stderr().strip()
                if not stderr:
                    stderr = "(no stderr captured)"
                raise InvalidEvidence(f"MCP server closed unexpectedly: {stderr}")
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise InvalidEvidence(f"MCP emitted invalid JSON: {line!r}") from error
            if value.get("id") == request_id:
                return value

    def initialize(self) -> float:
        started = time.perf_counter_ns()
        self._send(
            {
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "leantoken-agent-walltime-ab",
                        "version": "1",
                    },
                },
            }
        )
        response = self._response(0)
        if "result" not in response:
            raise InvalidEvidence(f"MCP initialize failed: {response}")
        elapsed = elapsed_ms(started)
        self._send(
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }
        )
        return elapsed

    def call(self, name: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        request_id = self.next_id
        self.next_id += 1
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        response = self._response(request_id)
        encoded_bytes = len(
            json.dumps(
                response, separators=(",", ":"), ensure_ascii=False
            ).encode("utf-8")
        )
        result = response.get("result")
        if not isinstance(result, dict):
            raise InvalidEvidence(f"{name} returned no MCP result: {response}")
        if result.get("isError") is True:
            raise InvalidEvidence(f"{name} returned an MCP error: {result}")
        structured = result.get("structuredContent")
        if not isinstance(structured, dict):
            raise InvalidEvidence(f"{name} omitted structuredContent")
        if structured.get("status") == "retryable":
            raise InvalidEvidence(f"{name} remained retryable during measurement")
        return structured, encoded_bytes

    def wait_ready(self, timeout_seconds: float = 30.0) -> float:
        started = time.perf_counter_ns()
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline:
            try:
                self.call(
                    "files",
                    {"operation": {"kind": "tree"}, "max_results": 1},
                )
                return elapsed_ms(started)
            except InvalidEvidence as error:
                if "retryable" not in str(error):
                    raise
            time.sleep(0.02)
        raise InvalidEvidence("MCP server did not become ready before the deadline")


def elapsed_ms(started_ns: int) -> float:
    return (time.perf_counter_ns() - started_ns) / 1_000_000


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise InvalidEvidence(f"cannot read {path}: {error}") from error


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_json(value: Any) -> str:
    payload = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def command_output(command: Sequence[str], cwd: Path | None = None) -> str:
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    if not completed.returncode == 0:
        raise InvalidEvidence(
            f"{' '.join(command)} failed: {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def optional_command_version(command: str) -> str | None:
    executable = shutil.which(command)
    if executable is None:
        return None
    completed = subprocess.run(
        [executable, "--version"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
    )
    first_line = completed.stdout.splitlines()
    return first_line[0].strip() if first_line else executable


def validate_relative_path(value: str) -> None:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or value in {"", "."}:
        raise InvalidEvidence(f"unsafe benchmark path {value!r}")


def normalize_path(value: str) -> str:
    normalized = value.replace("\\", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    return normalized


def line_start_offsets(path: Path) -> list[int]:
    content = path.read_bytes()
    starts = [0]
    starts.extend(index + 1 for index, byte in enumerate(content) if byte == 0x0A)
    return starts


def parse_leantoken_occurrences(
    response: dict[str, Any], root: Path
) -> list[Occurrence]:
    total = response.get("occurrences_total")
    returned = response.get("occurrences_returned")
    if not isinstance(total, int) or returned != total:
        raise InvalidEvidence(
            "LeanToken exhaustive search did not return every occurrence"
        )
    groups = response.get("groups")
    if isinstance(groups, list):
        occurrences: list[Occurrence] = []
        for group in groups:
            if not isinstance(group, dict) or not isinstance(
                group.get("occurrences"), list
            ):
                raise InvalidEvidence("LeanToken occurrence group omitted coordinates")
            path = normalize_path(str(group["path"]))
            validate_relative_path(path)
            if not (root / path).is_file():
                raise InvalidEvidence(f"LeanToken occurrence path is absent: {path}")
            for coordinate in group["occurrences"]:
                if not isinstance(coordinate, dict):
                    raise InvalidEvidence(f"invalid LeanToken coordinate in {path}")
                line = coordinate.get("line")
                end_line = coordinate.get("end_line", line)
                start_column = coordinate.get("start_column")
                end_column = coordinate.get("end_column")
                if (
                    not isinstance(line, int)
                    or end_line != line
                    or not isinstance(start_column, int)
                    or not isinstance(end_column, int)
                ):
                    raise InvalidEvidence(f"invalid LeanToken coordinate in {path}")
                occurrences.append(
                    Occurrence(path, line, start_column, end_column)
                )
        if len(occurrences) != total:
            raise InvalidEvidence(
                "LeanToken exhaustive search did not return every occurrence"
            )
        return sorted(occurrences)

    hits = response.get("hits")
    if not isinstance(hits, list) or total != len(hits):
        raise InvalidEvidence("LeanToken search response omitted occurrence evidence")
    offsets: dict[str, list[int]] = {}
    occurrences: list[Occurrence] = []
    for hit in hits:
        if not isinstance(hit, dict) or not isinstance(hit.get("occurrence"), dict):
            raise InvalidEvidence("LeanToken exhaustive hit omitted occurrence metadata")
        path = normalize_path(str(hit["path"]))
        validate_relative_path(path)
        occurrence = hit["occurrence"]
        line = occurrence.get("start_line")
        end_line = occurrence.get("end_line")
        start_byte = occurrence.get("start_byte")
        end_byte = occurrence.get("end_byte")
        if (
            not isinstance(line, int)
            or end_line != line
            or not isinstance(start_byte, int)
            or not isinstance(end_byte, int)
        ):
            raise InvalidEvidence(f"invalid LeanToken occurrence in {path}")
        starts = offsets.setdefault(path, line_start_offsets(root / path))
        if line < 1 or line > len(starts):
            raise InvalidEvidence(f"LeanToken occurrence line is outside {path}")
        base = starts[line - 1]
        occurrences.append(
            Occurrence(path, line, start_byte - base, end_byte - base)
        )
    return sorted(occurrences)


def run_rg_exact(root: Path, query: str) -> tuple[list[Occurrence], int]:
    completed = subprocess.run(
        [
            "rg",
            "--no-config",
            "--sort",
            "path",
            "--path-separator",
            "/",
            "--json",
            "--line-number",
            "--fixed-strings",
            "--",
            query,
            ".",
        ],
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.returncode not in (0, 1):
        raise InvalidEvidence(
            f"ripgrep failed for {query!r}: "
            f"{completed.stderr.decode('utf-8', errors='replace')}"
        )
    occurrences: list[Occurrence] = []
    for raw_line in completed.stdout.splitlines():
        event = json.loads(raw_line)
        if event.get("type") != "match":
            continue
        data = event["data"]
        path_data = data["path"]
        if "text" not in path_data:
            raise InvalidEvidence("ripgrep returned a non-UTF-8 path")
        path = normalize_path(path_data["text"])
        line = data["line_number"]
        for match in data["submatches"]:
            occurrences.append(
                Occurrence(path, line, match["start"], match["end"])
            )
    return sorted(occurrences), len(completed.stdout)


def run_sed_read(root: Path, path: str, start: int, end: int) -> tuple[str, int]:
    completed = subprocess.run(
        ["sed", "-n", f"{start},{end}p", path],
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.returncode != 0:
        raise InvalidEvidence(
            f"sed failed for {path}: "
            f"{completed.stderr.decode('utf-8', errors='replace')}"
        )
    try:
        content = completed.stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        raise InvalidEvidence(f"sed returned non-UTF-8 source for {path}") from error
    return content, len(completed.stdout)


def run_rg_discovery(
    root: Path, queries: Sequence[str]
) -> tuple[set[str], int, int]:
    paths: set[str] = set()
    output_bytes = 0
    matches = 0
    for query in queries:
        completed = subprocess.run(
            [
                "rg",
                "--no-config",
                "--sort",
                "path",
                "--path-separator",
                "/",
                "--json",
                "--line-number",
                "--fixed-strings",
                "--",
                query,
                ".",
            ],
            cwd=root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if completed.returncode not in (0, 1):
            raise InvalidEvidence(
                f"ripgrep failed for {query!r}: "
                f"{completed.stderr.decode('utf-8', errors='replace')}"
            )
        output_bytes += len(completed.stdout)
        for raw_line in completed.stdout.splitlines():
            event = json.loads(raw_line)
            if event.get("type") != "match":
                continue
            path_data = event["data"]["path"]
            if "text" not in path_data:
                raise InvalidEvidence("ripgrep returned a non-UTF-8 path")
            paths.add(normalize_path(path_data["text"]))
            matches += len(event["data"]["submatches"])
    return paths, matches, output_bytes


def canonical_context(response: dict[str, Any]) -> str:
    value = json.loads(json.dumps(response))
    meta = value.get("meta")
    if isinstance(meta, dict):
        meta.pop("receipt_id", None)
    receipt = value.get("receipt")
    if isinstance(receipt, dict):
        receipt.pop("receipt_id", None)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def percentile(values: Sequence[float], quantile: float) -> float:
    if not values:
        raise InvalidEvidence("cannot summarize an empty timing sample")
    ordered = sorted(values)
    rank = math.ceil(quantile * len(ordered)) - 1
    return ordered[max(0, min(rank, len(ordered) - 1))]


def stats(values: Sequence[float]) -> dict[str, Any]:
    return {
        "samples": len(values),
        "minimum_ms": min(values),
        "median_ms": statistics.median(values),
        "mean_ms": statistics.fmean(values),
        "p95_ms": percentile(values, 0.95),
        "maximum_ms": max(values),
    }


def ratio_delta(native_ms: float, leantoken_ms: float) -> dict[str, float]:
    if native_ms <= 0:
        raise InvalidEvidence("native median must be positive")
    return {
        "absolute_ms": leantoken_ms - native_ms,
        "relative": leantoken_ms / native_ms - 1.0,
        "ratio": leantoken_ms / native_ms,
    }


def measure_pair(
    iterations: int,
    native: Callable[[], tuple[Any, int]],
    leantoken: Callable[[], tuple[Any, int]],
    validate: Callable[[Any, Any], None],
) -> tuple[list[dict[str, Any]], Any, Any, int, int]:
    samples: list[dict[str, Any]] = []
    canonical_native: Any = None
    canonical_leantoken: Any = None
    native_bytes = 0
    leantoken_bytes = 0
    for index in range(iterations):
        order = "native-leantoken" if index % 2 == 0 else "leantoken-native"
        measured: dict[str, tuple[float, Any, int]] = {}
        operations = (
            (("native", native), ("leantoken", leantoken))
            if index % 2 == 0
            else (("leantoken", leantoken), ("native", native))
        )
        for label, operation in operations:
            started = time.perf_counter_ns()
            value, payload_bytes = operation()
            measured[label] = (elapsed_ms(started), value, payload_bytes)
        validate(measured["native"][1], measured["leantoken"][1])
        if canonical_native is None:
            canonical_native = measured["native"][1]
            canonical_leantoken = measured["leantoken"][1]
            native_bytes = measured["native"][2]
            leantoken_bytes = measured["leantoken"][2]
        elif (
            measured["native"][1] != canonical_native
            or measured["leantoken"][1] != canonical_leantoken
        ):
            raise InvalidEvidence("a measured operation returned nondeterministic output")
        samples.append(
            {
                "order": order,
                "native_ms": measured["native"][0],
                "leantoken_ms": measured["leantoken"][0],
            }
        )
    return (
        samples,
        canonical_native,
        canonical_leantoken,
        native_bytes,
        leantoken_bytes,
    )


def operation_report(
    samples: list[dict[str, Any]],
    native_bytes: int,
    leantoken_bytes: int,
) -> dict[str, Any]:
    native_stats = stats([sample["native_ms"] for sample in samples])
    leantoken_stats = stats([sample["leantoken_ms"] for sample in samples])
    return {
        "native": native_stats,
        "leantoken": leantoken_stats,
        "median_delta": ratio_delta(
            native_stats["median_ms"], leantoken_stats["median_ms"]
        ),
        "native_payload_bytes": native_bytes,
        "leantoken_payload_bytes": leantoken_bytes,
        "raw_samples": samples,
    }


def find_task(validation: dict[str, Any], corpus_name: str, task_id: str) -> tuple[
    dict[str, Any], dict[str, Any]
]:
    for corpus in validation.get("corpora", []):
        if corpus.get("name") != corpus_name:
            continue
        for task in corpus.get("tasks", []):
            if task.get("id") == task_id:
                return corpus, task
        raise InvalidEvidence(f"{corpus_name} has no task {task_id}")
    raise InvalidEvidence(f"validation manifest has no corpus {corpus_name}")


def git_revision(root: Path) -> str:
    return command_output(["git", "rev-parse", "HEAD"], cwd=root)


def verify_inputs(
    manifest_path: Path,
    validation_path: Path,
    manifest: dict[str, Any],
    validation: dict[str, Any],
    repos_root: Path,
) -> list[tuple[dict[str, Any], dict[str, Any], dict[str, Any], Path]]:
    if manifest.get("schema_version") != 1:
        raise InvalidEvidence("unsupported agent wall-time manifest schema")
    if validation.get("schema_version") != 2:
        raise InvalidEvidence("unsupported validation manifest schema")
    expected_source = manifest.get("source_manifest")
    if expected_source != "benchmarks/validation.json":
        raise InvalidEvidence("agent wall-time manifest source_manifest changed")
    expected_hash = manifest.get("source_manifest_sha256")
    if expected_hash != sha256_file(validation_path):
        raise InvalidEvidence("validation manifest SHA-256 does not match the freeze")
    if not manifest.get("corpora"):
        raise InvalidEvidence("agent wall-time manifest has no corpora")
    seen: set[str] = set()
    resolved = []
    for workload in manifest["corpora"]:
        name = workload.get("name")
        task_id = workload.get("task_id")
        if not isinstance(name, str) or name in seen:
            raise InvalidEvidence(f"invalid or duplicate workload name {name!r}")
        seen.add(name)
        if not isinstance(task_id, str):
            raise InvalidEvidence(f"{name}: task_id is required")
        corpus, task = find_task(validation, name, task_id)
        root = repos_root / corpus["directory"]
        if not root.is_dir():
            raise InvalidEvidence(f"{name}: checkout is missing at {root}")
        if git_revision(root) != corpus["base_revision"]:
            raise InvalidEvidence(f"{name}: checkout is not at the frozen revision")
        read_path = workload.get("read_path")
        if not isinstance(read_path, str):
            raise InvalidEvidence(f"{name}: read_path is required")
        validate_relative_path(read_path)
        if not (root / read_path).is_file():
            raise InvalidEvidence(f"{name}: read_path does not exist")
        for key in ("read_start_line", "read_end_line"):
            if not isinstance(workload.get(key), int) or workload[key] < 1:
                raise InvalidEvidence(f"{name}: {key} must be positive")
        if workload["read_start_line"] > workload["read_end_line"]:
            raise InvalidEvidence(f"{name}: read range is reversed")
        if not isinstance(workload.get("search_query"), str) or not workload[
            "search_query"
        ]:
            raise InvalidEvidence(f"{name}: search_query is required")
        resolved.append((workload, corpus, task, root))
    return resolved


def repository_quality(
    returned_paths: set[str],
    fragments: Sequence[dict[str, Any]] | None,
    task: dict[str, Any],
) -> dict[str, Any]:
    relevant = {item["path"] for item in task["relevant_files"]}
    relevant_found = relevant & returned_paths
    result = {
        "returned_paths": sorted(returned_paths),
        "relevant_files": sorted(relevant),
        "relevant_files_found": sorted(relevant_found),
        "relevant_file_recall": len(relevant_found) / len(relevant),
        "labeled_file_precision": (
            len(relevant_found) / len(returned_paths) if returned_paths else 0.0
        ),
    }
    if fragments is None:
        result.update(
            {
                "line_anchors": None,
                "line_anchors_found": None,
                "line_anchor_recall": None,
            }
        )
        return result
    anchors = 0
    anchors_found = 0
    for item in task["relevant_files"]:
        for anchor in item.get("line_anchors", []):
            anchors += 1
            if any(
                fragment.get("path") == item["path"]
                and fragment.get("start_line", 0) <= anchor
                and fragment.get("end_line", 0) >= anchor
                for fragment in fragments
            ):
                anchors_found += 1
    result.update(
        {
            "line_anchors": anchors,
            "line_anchors_found": anchors_found,
            "line_anchor_recall": anchors_found / anchors if anchors else None,
        }
    )
    return result


def run_corpus(
    binary: Path,
    workload: dict[str, Any],
    corpus: dict[str, Any],
    task: dict[str, Any],
    root: Path,
    manifest: dict[str, Any],
    work_root: Path,
) -> dict[str, Any]:
    database = work_root / f"{corpus['directory']}.sqlite"
    started = time.perf_counter_ns()
    completed = subprocess.run(
        [
            str(binary),
            "index",
            "--root",
            str(root),
            "--database",
            str(database),
            "--json",
        ],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    cold_index_ms = elapsed_ms(started)
    if completed.returncode != 0:
        raise InvalidEvidence(
            f"{workload['name']}: cold index failed: "
            f"{completed.stderr.decode('utf-8', errors='replace')}"
        )
    database_bytes = sum(
        path.stat().st_size
        for path in database.parent.glob(f"{database.name}*")
        if path.is_file()
    )
    mcp = McpProcess(binary, root, database)
    try:
        initialize_ms = mcp.initialize()
        ready_ms = mcp.wait_ready()
        search_args = {
            "query": workload["search_query"],
            "mode": "text",
            "all_occurrences": True,
            "case_sensitive": True,
            "context_lines": 0,
            "max_results": manifest["max_search_results"],
        }
        read_args = {
            "path": workload["read_path"],
            "target": {
                "kind": "lines",
                "start": workload["read_start_line"],
                "end": workload["read_end_line"],
            },
        }
        context_args = {
            "task": task["prompt"],
            "token_budget": task["token_budget"],
            "max_fragments": manifest["context_max_fragments"],
        }

        def native_search() -> tuple[Any, int]:
            return run_rg_exact(root, workload["search_query"])

        def lean_search() -> tuple[Any, int]:
            response, payload = mcp.call("search", search_args)
            return parse_leantoken_occurrences(response, root), payload

        def validate_search(native: Any, lean: Any) -> None:
            if native != lean:
                raise InvalidEvidence(
                    f"{workload['name']}: exhaustive search occurrence mismatch"
                )

        def native_read() -> tuple[Any, int]:
            return run_sed_read(
                root,
                workload["read_path"],
                workload["read_start_line"],
                workload["read_end_line"],
            )

        def lean_read() -> tuple[Any, int]:
            response, payload = mcp.call("read", read_args)
            if response.get("status") != "content" or response.get("truncated"):
                raise InvalidEvidence(
                    f"{workload['name']}: exact read was incomplete"
                )
            if (
                response.get("start_line") != workload["read_start_line"]
                or response.get("end_line") != workload["read_end_line"]
            ):
                raise InvalidEvidence(
                    f"{workload['name']}: exact read coordinates changed"
                )
            return response.get("content"), payload

        def validate_read(native: Any, lean: Any) -> None:
            if native != lean:
                raise InvalidEvidence(
                    f"{workload['name']}: exact read content mismatch"
                )

        def native_discovery() -> tuple[Any, int]:
            paths, matches, payload = run_rg_discovery(root, task["rg_queries"])
            return {"paths": sorted(paths), "matches": matches}, payload

        def lean_context() -> tuple[Any, int]:
            response, payload = mcp.call("context", context_args)
            fragments = response.get("fragments")
            meta = response.get("meta")
            if not isinstance(fragments, list) or not isinstance(meta, dict):
                raise InvalidEvidence(
                    f"{workload['name']}: context response is incomplete"
                )
            emitted = meta.get("emitted_tokens")
            if not isinstance(emitted, int) or emitted > task["token_budget"]:
                raise InvalidEvidence(
                    f"{workload['name']}: context exceeded its token budget"
                )
            return {
                "canonical": canonical_context(response),
                "paths": sorted({item["path"] for item in fragments}),
                "fragments": fragments,
                "source_tokens": emitted,
                "payload_tokens": meta.get("payload_tokens"),
            }, payload

        def validate_discovery(native: Any, lean: Any) -> None:
            if not isinstance(native.get("paths"), list) or not isinstance(
                lean.get("paths"), list
            ):
                raise InvalidEvidence(
                    f"{workload['name']}: discovery output is incomplete"
                )

        for _ in range(manifest["warmups"]):
            native_search()
            lean_search()
            native_read()
            lean_read()
            native_discovery()
            lean_context()

        search = measure_pair(
            manifest["iterations"], native_search, lean_search, validate_search
        )
        read = measure_pair(
            manifest["iterations"], native_read, lean_read, validate_read
        )
        discovery = measure_pair(
            manifest["context_iterations"],
            native_discovery,
            lean_context,
            validate_discovery,
        )
    finally:
        mcp.close()

    search_report = operation_report(search[0], search[3], search[4])
    search_report["occurrences"] = len(search[1])
    search_report["observable_sha256"] = sha256_json(
        [
            {
                "path": occurrence.path,
                "line": occurrence.line,
                "start_column_byte": occurrence.start_column_byte,
                "end_column_byte": occurrence.end_column_byte,
            }
            for occurrence in search[1]
        ]
    )
    search_report["parity"] = "pass"
    read_report = operation_report(read[0], read[3], read[4])
    read_report["returned_lines"] = workload["read_end_line"] - workload[
        "read_start_line"
    ] + 1
    read_report["observable_sha256"] = hashlib.sha256(
        read[1].encode("utf-8")
    ).hexdigest()
    read_report["parity"] = "pass"
    discovery_report = operation_report(discovery[0], discovery[3], discovery[4])
    native_paths = set(discovery[1]["paths"])
    context_value = discovery[2]
    context_paths = set(context_value["paths"])
    discovery_report["semantic_relation"] = "different-capability-diagnostic"
    discovery_report["native_match_count"] = discovery[1]["matches"]
    discovery_report["native_observable_sha256"] = sha256_json(discovery[1])
    discovery_report["context_observable_sha256"] = hashlib.sha256(
        context_value["canonical"].encode("utf-8")
    ).hexdigest()
    discovery_report["native_quality"] = repository_quality(native_paths, None, task)
    discovery_report["context_quality"] = repository_quality(
        context_paths, context_value["fragments"], task
    )
    discovery_report["context_source_tokens"] = context_value["source_tokens"]
    discovery_report["context_payload_tokens"] = context_value["payload_tokens"]
    discovery_report["context_determinism"] = "pass"
    return {
        "name": workload["name"],
        "directory": corpus["directory"],
        "base_revision": corpus["base_revision"],
        "task_id": task["id"],
        "search_query": workload["search_query"],
        "read_target": {
            "path": workload["read_path"],
            "start_line": workload["read_start_line"],
            "end_line": workload["read_end_line"],
        },
        "rg_queries": task["rg_queries"],
        "token_budget": task["token_budget"],
        "cold_index_ms": cold_index_ms,
        "database_bytes": database_bytes,
        "mcp_initialize_ms": initialize_ms,
        "mcp_ready_after_initialized_ms": ready_ms,
        "exact_search": search_report,
        "exact_read": read_report,
        "discovery_context": discovery_report,
    }


def aggregate(corpora: Sequence[dict[str, Any]]) -> dict[str, Any]:
    relevant_files = sum(
        len(item["discovery_context"]["context_quality"]["relevant_files"])
        for item in corpora
    )
    native_relevant = sum(
        len(item["discovery_context"]["native_quality"]["relevant_files_found"])
        for item in corpora
    )
    context_relevant = sum(
        len(item["discovery_context"]["context_quality"]["relevant_files_found"])
        for item in corpora
    )
    line_anchors = sum(
        item["discovery_context"]["context_quality"]["line_anchors"]
        for item in corpora
    )
    context_line_anchors = sum(
        item["discovery_context"]["context_quality"]["line_anchors_found"]
        for item in corpora
    )
    result: dict[str, Any] = {
        "corpus_count": len(corpora),
        "accuracy_gates": {
            "exact_search_parity": "pass",
            "exact_read_parity": "pass",
            "context_determinism": "pass",
            "context_token_budgets": "pass",
        },
        "quality": {
            "relevant_files": relevant_files,
            "native_discovery_relevant_files_found": native_relevant,
            "native_discovery_relevant_file_recall": native_relevant / relevant_files,
            "context_relevant_files_found": context_relevant,
            "context_relevant_file_recall": context_relevant / relevant_files,
            "context_line_anchors": line_anchors,
            "context_line_anchors_found": context_line_anchors,
            "context_line_anchor_recall": context_line_anchors / line_anchors,
        },
        "cold_index_ms": stats([item["cold_index_ms"] for item in corpora]),
        "mcp_initialize_ms": stats([item["mcp_initialize_ms"] for item in corpora]),
        "mcp_ready_after_initialized_ms": stats(
            [item["mcp_ready_after_initialized_ms"] for item in corpora]
        ),
    }
    for report_key, aggregate_key in (
        ("exact_search", "exact_search"),
        ("exact_read", "exact_read"),
        ("discovery_context", "discovery_context"),
    ):
        native_sum = sum(
            item[report_key]["native"]["median_ms"] for item in corpora
        )
        leantoken_sum = sum(
            item[report_key]["leantoken"]["median_ms"] for item in corpora
        )
        result[aggregate_key] = {
            "sum_of_corpus_medians": {
                "native_ms": native_sum,
                "leantoken_ms": leantoken_sum,
                "delta": ratio_delta(native_sum, leantoken_sum),
            }
        }
    return result


def markdown_report(report: dict[str, Any]) -> str:
    aggregate = report["aggregate"]
    quality = aggregate["quality"]
    search_suite = aggregate["exact_search"]["sum_of_corpus_medians"]
    read_suite = aggregate["exact_read"]["sum_of_corpus_medians"]
    discovery_suite = aggregate["discovery_context"]["sum_of_corpus_medians"]
    lines = [
        "# Agent wall-time A/B",
        "",
        f"- Status: `{report['status']}`",
        f"- Source: `{report['provenance']['source_revision']}`",
        f"- Host: `{report['provenance']['host_os']}/{report['provenance']['host_arch']}`",
        f"- Iterations: {report['protocol']['iterations']} exact, "
        f"{report['protocol']['context_iterations']} context",
        "",
        "Exact search and read are parity-gated. Discovery and context have different",
        "semantics and their timing ratio is diagnostic only.",
        "",
        "| Corpus | Cold index | Exact search rg | Exact search MCP | Exact read native | Exact read MCP | Discovery rg | Context MCP |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for corpus in report["corpora"]:
        lines.append(
            "| {name} | {cold:.1f} ms | {search_native:.2f} ms | "
            "{search_lean:.2f} ms | {read_native:.2f} ms | {read_lean:.2f} ms | "
            "{discovery_native:.2f} ms | {context:.2f} ms |".format(
                name=corpus["name"],
                cold=corpus["cold_index_ms"],
                search_native=corpus["exact_search"]["native"]["median_ms"],
                search_lean=corpus["exact_search"]["leantoken"]["median_ms"],
                read_native=corpus["exact_read"]["native"]["median_ms"],
                read_lean=corpus["exact_read"]["leantoken"]["median_ms"],
                discovery_native=corpus["discovery_context"]["native"]["median_ms"],
                context=corpus["discovery_context"]["leantoken"]["median_ms"],
            )
        )
    lines.extend(
        [
            "",
        "## Accuracy",
        "",
        "- Exhaustive exact-search coordinate parity: pass",
        "- Exact line-read parity: pass",
        "- Warm context determinism and token budgets: pass",
        "- Native discovery relevant-file recall: "
        f"{quality['native_discovery_relevant_files_found']}/"
        f"{quality['relevant_files']} "
        f"({quality['native_discovery_relevant_file_recall']:.1%})",
        "- Context relevant-file recall: "
        f"{quality['context_relevant_files_found']}/"
        f"{quality['relevant_files']} "
        f"({quality['context_relevant_file_recall']:.1%})",
        "- Context line-anchor recall: "
        f"{quality['context_line_anchors_found']}/"
        f"{quality['context_line_anchors']} "
        f"({quality['context_line_anchor_recall']:.1%})",
        "",
        "## Suite Diagnostic",
        "",
        "These are sums of the four corpus medians, not pooled latency samples.",
        "",
        "| Operation | Native | LeanToken | Absolute delta | Relative delta |",
        "| --- | ---: | ---: | ---: | ---: |",
        "| Exhaustive exact search | "
        f"{search_suite['native_ms']:.2f} ms | "
        f"{search_suite['leantoken_ms']:.2f} ms | "
        f"{search_suite['delta']['absolute_ms']:+.2f} ms | "
        f"{search_suite['delta']['relative']:+.1%} |",
        "| Exact read | "
        f"{read_suite['native_ms']:.2f} ms | "
        f"{read_suite['leantoken_ms']:.2f} ms | "
        f"{read_suite['delta']['absolute_ms']:+.2f} ms | "
        f"{read_suite['delta']['relative']:+.1%} |",
        "| Discovery / context diagnostic | "
        f"{discovery_suite['native_ms']:.2f} ms | "
        f"{discovery_suite['leantoken_ms']:.2f} ms | "
        f"{discovery_suite['delta']['absolute_ms']:+.2f} ms | "
        f"{discovery_suite['delta']['relative']:+.1%} |",
        "",
        "## Limits",
            "",
        ]
    )
    lines.extend(f"- {item}" for item in report["limitations"])
    return "\n".join(lines) + "\n"


def run(args: argparse.Namespace) -> None:
    manifest_path = Path(args.manifest).resolve()
    validation_path = Path(args.validation_manifest).resolve()
    repos_root = Path(args.repos_root).resolve()
    binary = Path(args.leantoken).resolve()
    output = Path(args.output).resolve()
    markdown_output = Path(args.markdown_output).resolve()
    manifest = load_json(manifest_path)
    validation = load_json(validation_path)
    if not isinstance(manifest, dict) or not isinstance(validation, dict):
        raise InvalidEvidence("benchmark manifests must be JSON objects")
    if args.iterations is not None:
        manifest["iterations"] = args.iterations
    if args.context_iterations is not None:
        manifest["context_iterations"] = args.context_iterations
    if args.warmups is not None:
        manifest["warmups"] = args.warmups
    for key in ("iterations", "context_iterations", "warmups"):
        if not isinstance(manifest.get(key), int) or manifest[key] < 1:
            raise InvalidEvidence(f"{key} must be a positive integer")
    if not binary.is_file():
        raise InvalidEvidence(f"LeanToken binary is missing: {binary}")
    for command in ("git", "rg", "sed", "rustc"):
        if shutil.which(command) is None:
            raise InvalidEvidence(f"required command {command!r} was not found")
    resolved = verify_inputs(
        manifest_path,
        validation_path,
        manifest,
        validation,
        repos_root,
    )
    if args.preflight_only:
        print(
            json.dumps(
                {
                    "status": "preflight_passed",
                    "corpora": [item[0]["name"] for item in resolved],
                },
                separators=(",", ":"),
            )
        )
        return
    source_root = Path(args.source_root).resolve()
    source_revision = git_revision(source_root)
    source_tree = command_output(["git", "rev-parse", "HEAD^{tree}"], cwd=source_root)
    dirty = bool(
        command_output(
            ["git", "status", "--porcelain", "--untracked-files=all"],
            cwd=source_root,
        )
    )
    if dirty:
        raise InvalidEvidence("formal agent wall-time runs require a clean source tree")
    with tempfile.TemporaryDirectory(prefix="leantoken-agent-walltime-ab-") as directory:
        work_root = Path(directory)
        corpora = [
            run_corpus(binary, workload, corpus, task, root, manifest, work_root)
            for workload, corpus, task, root in resolved
        ]
    limitations = [
        "This is a local retrieval microbenchmark, not an end-to-end agent task-time result.",
        "Exact search and exact read are observable-parity comparisons; multi-query ripgrep discovery and ranked context are not semantically equivalent.",
        "Wall time depends on this host, filesystem cache, process scheduler, and pinned corpus sizes.",
        "Cold index cost is paid once per repository generation and must be amortized over a real session.",
        "CPU time, peak RSS, provider latency, model turns, patch quality, and task success are outside this report.",
        "The prospective validation tasks are consumed development evidence rather than a blind holdout.",
    ]
    report = {
        "schema_version": 1,
        "report_kind": "agent_walltime_ab",
        "status": "passed_accuracy_gates",
        "hypothesis": (
            "Persistent indexed exact operations should remain within small absolute "
            "wall-time overhead while preserving exact search/read results; ranked "
            "context cost is measured separately and must not be interpreted as an "
            "equivalent ripgrep operation."
        ),
        "provenance": {
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "source_revision": source_revision,
            "source_tree": source_tree,
            "source_dirty": dirty,
            "binary_sha256": sha256_file(binary),
            "manifest_sha256": sha256_file(manifest_path),
            "validation_manifest_sha256": sha256_file(validation_path),
            "leantoken_version": command_output([str(binary), "--version"]),
            "rustc_version": command_output(["rustc", "--version"]),
            "ripgrep_version": optional_command_version("rg"),
            "sed_version": optional_command_version("sed"),
            "python_version": platform.python_version(),
            "host_os": platform.system().lower(),
            "host_arch": platform.machine(),
            "cpu": platform.processor() or None,
        },
        "protocol": {
            "build_profile": "release",
            "transport": "persistent newline-delimited MCP stdio",
            "result_mode": "structured",
            "sample_order": "alternating native-leantoken / leantoken-native",
            "iterations": manifest["iterations"],
            "context_iterations": manifest["context_iterations"],
            "warmups": manifest["warmups"],
            "accuracy_gates": manifest["accuracy_gates"],
            "source_manifest": manifest["source_manifest"],
        },
        "aggregate": aggregate(corpora),
        "corpora": corpora,
        "limitations": limitations,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    markdown_output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    markdown_output.write_text(markdown_report(report), encoding="utf-8")
    print(json.dumps({"status": report["status"], "output": str(output)}))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest", default="benchmarks/agent_walltime_ab.json"
    )
    parser.add_argument(
        "--validation-manifest", default="benchmarks/validation.json"
    )
    parser.add_argument("--repos-root", default="target/validation-repos")
    parser.add_argument("--source-root", default=".")
    parser.add_argument("--leantoken", default="target/release/leantoken")
    parser.add_argument(
        "--output", default="target/agent-walltime-ab/report.json"
    )
    parser.add_argument(
        "--markdown-output", default="target/agent-walltime-ab/report.md"
    )
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--context-iterations", type=int)
    parser.add_argument("--warmups", type=int)
    parser.add_argument("--preflight-only", action="store_true")
    return parser


def main() -> int:
    try:
        run(build_parser().parse_args())
    except InvalidEvidence as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
