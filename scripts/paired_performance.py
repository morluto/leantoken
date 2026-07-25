#!/usr/bin/env python3
"""Adapt paired LeanToken JSON samples into Benchstat input and gate reports."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import re
import sys
from pathlib import Path
from typing import Any


SHA1 = re.compile(r"^[0-9a-f]{40}$")
BENCHSTAT_CHANGE = re.compile(r"^[+-](?:\d+(?:\.\d*)?|\.\d+)%$")
UNIT_TO_NS = {"ns": 1.0, "us": 1_000.0, "ms": 1_000_000.0, "s": 1_000_000_000.0}


class InvalidEvidence(ValueError):
    """Raised when paired benchmark evidence is incomplete or inconsistent."""


def json_pointer(value: Any, pointer: str) -> Any:
    if pointer == "":
        return value
    if not pointer.startswith("/"):
        raise InvalidEvidence(f"invalid JSON pointer {pointer!r}")
    current = value
    for raw_part in pointer[1:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict) and part in current:
            current = current[part]
        elif isinstance(current, list) and part.isdigit() and int(part) < len(current):
            current = current[int(part)]
        else:
            raise InvalidEvidence(f"JSON pointer {pointer!r} does not exist")
    return current


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise InvalidEvidence(f"cannot read {path}: {error}") from error


def canonical_digest(value: Any) -> str:
    payload = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def benchmark_line(name: str, value: Any, source_unit: str) -> str:
    if source_unit not in UNIT_TO_NS:
        raise InvalidEvidence(f"{name}: unsupported source unit {source_unit!r}")
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise InvalidEvidence(f"{name}: metric must be numeric")
    number = float(value)
    if not math.isfinite(number):
        raise InvalidEvidence(f"{name}: metric must be finite")
    nanoseconds = int(round(number * UNIT_TO_NS[source_unit]))
    if nanoseconds < 0:
        raise InvalidEvidence(f"{name}: metric must not be negative")
    return f"Benchmark{name} 1 {nanoseconds} ns/op"


def validate_provenance(
    provenance: dict[str, Any],
    *,
    side: str,
    pair: int,
    rustc_prefix: str,
    benchstat_version: str,
) -> None:
    expected_order = "AB" if pair % 2 == 1 else "BA"
    expected_sequence = 1 if (side == "base") == (expected_order == "AB") else 2
    expected = {
        "schema_version": 1,
        "side": side,
        "pair": pair,
        "order": expected_order,
        "sequence": expected_sequence,
        "source_dirty": False,
        "benchstat_version": benchstat_version,
    }
    for field, expected_value in expected.items():
        if provenance.get(field) != expected_value:
            raise InvalidEvidence(
                f"pair {pair} {side}: {field}={provenance.get(field)!r}, "
                f"expected {expected_value!r}"
            )
    for field in ("source_sha", "source_tree_sha"):
        value = provenance.get(field)
        if not isinstance(value, str) or not SHA1.fullmatch(value):
            raise InvalidEvidence(f"pair {pair} {side}: invalid {field}")
    rustc_version = provenance.get("rustc_version")
    if not isinstance(rustc_version, str) or not rustc_version.startswith(rustc_prefix):
        raise InvalidEvidence(
            f"pair {pair} {side}: rustc_version does not start with {rustc_prefix!r}"
        )
    for field in ("host_os", "host_arch"):
        if not isinstance(provenance.get(field), str) or not provenance[field]:
            raise InvalidEvidence(f"pair {pair} {side}: missing {field}")


def collect(args: argparse.Namespace) -> None:
    manifest_path = Path(args.manifest)
    manifest = load_json(manifest_path)
    if manifest.get("schema_version") != 1:
        raise InvalidEvidence("unsupported paired-performance manifest schema")
    reports_config = manifest.get("reports")
    metrics = manifest.get("metrics")
    if not isinstance(reports_config, dict) or not reports_config:
        raise InvalidEvidence("manifest reports must be a non-empty object")
    if not isinstance(metrics, list) or not metrics:
        raise InvalidEvidence("manifest metrics must be a non-empty array")
    rustc_prefix = manifest.get("rustc_version_prefix")
    if not isinstance(rustc_prefix, str) or not rustc_prefix:
        raise InvalidEvidence("manifest rustc_version_prefix is required")
    benchstat_version = manifest.get("benchstat_version")
    if not isinstance(benchstat_version, str) or not benchstat_version:
        raise InvalidEvidence("manifest benchstat_version is required")

    samples_root = Path(args.samples)
    bench_lines: dict[str, list[str]] = {"base": [], "head": []}
    sources: dict[str, set[str]] = {"base": set(), "head": set()}
    source_trees: dict[str, set[str]] = {"base": set(), "head": set()}
    rustc_versions: set[str] = set()
    hosts: set[tuple[str, str]] = set()
    parity_digests: dict[str, set[str]] = {}

    for pair in range(1, args.pairs + 1):
        for side in ("base", "head"):
            sample_root = samples_root / f"{side}-{pair:02d}"
            provenance = load_json(sample_root / "provenance.json")
            if not isinstance(provenance, dict):
                raise InvalidEvidence(
                    f"pair {pair} {side}: provenance must be an object"
                )
            validate_provenance(
                provenance,
                side=side,
                pair=pair,
                rustc_prefix=rustc_prefix,
                benchstat_version=benchstat_version,
            )
            sources[side].add(provenance["source_sha"])
            source_trees[side].add(provenance["source_tree_sha"])
            rustc_versions.add(provenance["rustc_version"])
            hosts.add((provenance["host_os"], provenance["host_arch"]))

            reports: dict[str, Any] = {}
            for report_name, report_config in reports_config.items():
                if not isinstance(report_config, dict):
                    raise InvalidEvidence(
                        f"report {report_name}: configuration must be an object"
                    )
                filename = report_config.get("file")
                if not isinstance(filename, str) or not filename:
                    raise InvalidEvidence(f"report {report_name}: file is required")
                report = load_json(sample_root / filename)
                reports[report_name] = report
                required = report_config.get("required", {})
                if not isinstance(required, dict):
                    raise InvalidEvidence(
                        f"report {report_name}: required must be an object"
                    )
                for pointer, expected in required.items():
                    actual = json_pointer(report, pointer)
                    if actual != expected:
                        raise InvalidEvidence(
                            f"pair {pair} {side} {report_name}{pointer}: "
                            f"{actual!r}, expected {expected!r}"
                        )
                parity_pointers = report_config.get("parity", [])
                if not isinstance(parity_pointers, list):
                    raise InvalidEvidence(
                        f"report {report_name}: parity must be an array"
                    )
                for pointer in parity_pointers:
                    if not isinstance(pointer, str):
                        raise InvalidEvidence(
                            f"report {report_name}: parity pointers must be strings"
                        )
                    key = f"{report_name}{pointer}"
                    parity_digests.setdefault(key, set()).add(
                        canonical_digest(json_pointer(report, pointer))
                    )

            for metric in metrics:
                if not isinstance(metric, dict):
                    raise InvalidEvidence("metric configuration must be an object")
                benchmark = metric.get("benchmark")
                report_name = metric.get("report")
                pointer = metric.get("pointer")
                source_unit = metric.get("source_unit")
                if not all(
                    isinstance(value, str) and value
                    for value in (benchmark, report_name, pointer, source_unit)
                ):
                    raise InvalidEvidence(
                        "metric benchmark, report, pointer, and source_unit are required"
                    )
                if report_name not in reports:
                    raise InvalidEvidence(
                        f"{benchmark}: unknown report {report_name!r}"
                    )
                bench_lines[side].append(
                    benchmark_line(
                        benchmark,
                        json_pointer(reports[report_name], pointer),
                        source_unit,
                    )
                )

    for label, values in (
        ("base source", sources["base"]),
        ("head source", sources["head"]),
        ("base source tree", source_trees["base"]),
        ("head source tree", source_trees["head"]),
        ("rustc version", rustc_versions),
        ("host", hosts),
    ):
        if len(values) != 1:
            raise InvalidEvidence(f"{label} changed across samples: {sorted(values)}")
    if next(iter(sources["base"])) == next(iter(sources["head"])):
        raise InvalidEvidence("base and head source commits are identical")
    mismatched_parity = sorted(
        key for key, values in parity_digests.items() if len(values) != 1
    )
    if mismatched_parity:
        raise InvalidEvidence(
            f"observable parity mismatch: {', '.join(mismatched_parity)}"
        )

    host_os, host_arch = next(iter(hosts))
    header = [
        f"goos: {host_os}",
        f"goarch: {host_arch}",
        "pkg: leantoken/paired-performance",
    ]
    for side, output in (("base", Path(args.base_out)), ("head", Path(args.head_out))):
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            "\n".join([*header, *bench_lines[side]]) + "\n", encoding="utf-8"
        )

    manifest_sha256 = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    receipt = {
        "schema_version": 1,
        "pairs": args.pairs,
        "parity": "pass",
        "parity_fields": sorted(parity_digests),
        "manifest_sha256": manifest_sha256,
        "base_source_sha": next(iter(sources["base"])),
        "base_source_tree_sha": next(iter(source_trees["base"])),
        "head_source_sha": next(iter(sources["head"])),
        "head_source_tree_sha": next(iter(source_trees["head"])),
        "rustc_version": next(iter(rustc_versions)),
        "benchstat_version": benchstat_version,
        "host_os": host_os,
        "host_arch": host_arch,
    }
    parity_out = Path(args.parity_out)
    parity_out.parent.mkdir(parents=True, exist_ok=True)
    parity_out.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def read_benchstat_csv(path: Path) -> dict[str, dict[str, str]]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise InvalidEvidence(f"cannot read {path}: {error}") from error
    header_index = next(
        (
            index
            for index, line in enumerate(lines)
            if line.startswith(",") and "sec/op" in line and "vs base" in line
        ),
        None,
    )
    if header_index is None:
        raise InvalidEvidence("Benchstat CSV comparison header is missing")
    rows: dict[str, dict[str, str]] = {}
    for values in csv.reader(lines[header_index + 1 :]):
        if not values or not values[0] or values[0] == "geomean":
            continue
        if len(values) != 7:
            raise InvalidEvidence(f"unexpected Benchstat CSV row: {values!r}")
        name = values[0]
        if name in rows:
            raise InvalidEvidence(f"duplicate Benchstat benchmark {name}")
        rows[name] = {
            "base_seconds": values[1],
            "base_ci": values[2],
            "head_seconds": values[3],
            "head_ci": values[4],
            "comparison": values[5],
            "p": values[6],
        }
    if not rows:
        raise InvalidEvidence("Benchstat CSV contains no benchmark rows")
    return rows


def gate(args: argparse.Namespace) -> bool:
    manifest_path = Path(args.manifest)
    benchstat_path = Path(args.benchstat_csv)
    manifest = load_json(manifest_path)
    if manifest.get("schema_version") != 1:
        raise InvalidEvidence("unsupported paired-performance manifest schema")
    metrics = manifest.get("metrics")
    if not isinstance(metrics, list) or not metrics:
        raise InvalidEvidence("manifest metrics must be a non-empty array")
    configurations: dict[str, dict[str, Any]] = {}
    for metric in metrics:
        if not isinstance(metric, dict):
            raise InvalidEvidence("metric configuration must be an object")
        name = metric.get("benchmark")
        maximum = metric.get("max_regression_percent")
        minimum = metric.get("min_absolute_regression_ns")
        if not isinstance(name, str) or not name:
            raise InvalidEvidence("metric benchmark is required")
        if name in configurations:
            raise InvalidEvidence(f"duplicate metric benchmark {name}")
        if (
            isinstance(maximum, bool)
            or not isinstance(maximum, (int, float))
            or maximum < 0
        ):
            raise InvalidEvidence(
                f"{name}: max_regression_percent must not be negative"
            )
        if isinstance(minimum, bool) or not isinstance(minimum, int) or minimum < 0:
            raise InvalidEvidence(
                f"{name}: min_absolute_regression_ns must be a non-negative integer"
            )
        configurations[name] = metric

    benchstat_rows = read_benchstat_csv(benchstat_path)
    missing = sorted(set(configurations) - set(benchstat_rows))
    extra = sorted(set(benchstat_rows) - set(configurations))
    if missing or extra:
        raise InvalidEvidence(
            f"Benchstat metric set differs: missing={missing}, extra={extra}"
        )

    rows = []
    failures = []
    for name, config in configurations.items():
        raw = benchstat_rows[name]
        try:
            base_seconds = float(raw["base_seconds"])
            head_seconds = float(raw["head_seconds"])
        except ValueError as error:
            raise InvalidEvidence(f"{name}: invalid Benchstat seconds value") from error
        if (
            not math.isfinite(base_seconds)
            or not math.isfinite(head_seconds)
            or base_seconds <= 0
            or head_seconds <= 0
        ):
            raise InvalidEvidence(
                f"{name}: Benchstat seconds values must be positive and finite"
            )
        comparison = raw["comparison"]
        if comparison != "~" and not BENCHSTAT_CHANGE.fullmatch(comparison):
            raise InvalidEvidence(
                f"{name}: invalid Benchstat comparison {comparison!r}"
            )
        delta_percent = ((head_seconds - base_seconds) / base_seconds) * 100.0
        absolute_delta_ns = int(round((head_seconds - base_seconds) * 1_000_000_000.0))
        significant = comparison != "~"
        percentage_exceeded = delta_percent > float(config["max_regression_percent"])
        absolute_exceeded = absolute_delta_ns > int(
            config["min_absolute_regression_ns"]
        )
        if percentage_exceeded and absolute_exceeded and significant:
            status = "FAIL"
            failures.append(name)
        elif percentage_exceeded and absolute_exceeded:
            status = "INCONCLUSIVE"
        elif percentage_exceeded:
            status = "NOISE"
        else:
            status = "OK"
        rows.append(
            {
                "benchmark": name,
                "base_ns": int(round(base_seconds * 1_000_000_000.0)),
                "head_ns": int(round(head_seconds * 1_000_000_000.0)),
                "delta_percent": delta_percent,
                "absolute_delta_ns": absolute_delta_ns,
                "statistically_significant": significant,
                "benchstat_p": raw["p"],
                "base_ci": raw["base_ci"],
                "head_ci": raw["head_ci"],
                "max_regression_percent": config["max_regression_percent"],
                "min_absolute_regression_ns": config["min_absolute_regression_ns"],
                "status": status,
            }
        )

    decision = "fail" if failures else "pass"
    report = {
        "schema_version": 1,
        "decision": decision,
        "failures": failures,
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "benchstat_csv_sha256": hashlib.sha256(benchstat_path.read_bytes()).hexdigest(),
        "rows": rows,
    }
    json_out = Path(args.json_out)
    json_out.parent.mkdir(parents=True, exist_ok=True)
    json_out.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    markdown_lines = [
        "## Paired performance gate",
        "",
        "Benchstat supplies medians, confidence intervals, and significance. "
        "A row fails only when its percentage and absolute thresholds are both exceeded "
        "and Benchstat reports a significant change.",
        "",
        "| Benchmark | Base median (ns) | Head median (ns) | Delta | Abs delta (ns) | Significant | Status |",
        "| --- | ---: | ---: | ---: | ---: | --- | --- |",
    ]
    for row in rows:
        markdown_lines.append(
            f"| `{row['benchmark']}` | {row['base_ns']} | {row['head_ns']} | "
            f"{row['delta_percent']:+.2f}% | {row['absolute_delta_ns']:+d} | "
            f"{'yes' if row['statistically_significant'] else 'no'} | {row['status']} |"
        )
    markdown_lines.extend(["", f"Decision: **{decision.upper()}**", ""])
    markdown_out = Path(args.markdown_out)
    markdown_out.parent.mkdir(parents=True, exist_ok=True)
    markdown_out.write_text("\n".join(markdown_lines), encoding="utf-8")
    return not failures


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    collect_parser = subparsers.add_parser(
        "collect", help="validate paired samples and emit Benchstat inputs"
    )
    collect_parser.add_argument("--manifest", required=True)
    collect_parser.add_argument("--samples", required=True)
    collect_parser.add_argument("--pairs", required=True, type=int)
    collect_parser.add_argument("--base-out", required=True)
    collect_parser.add_argument("--head-out", required=True)
    collect_parser.add_argument("--parity-out", required=True)
    gate_parser = subparsers.add_parser(
        "gate", help="apply materiality thresholds to Benchstat CSV output"
    )
    gate_parser.add_argument("--manifest", required=True)
    gate_parser.add_argument("--benchstat-csv", required=True)
    gate_parser.add_argument("--markdown-out", required=True)
    gate_parser.add_argument("--json-out", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "collect":
            if args.pairs < 2:
                raise InvalidEvidence("--pairs must be at least 2")
            collect(args)
        elif args.command == "gate":
            return 0 if gate(args) else 1
    except InvalidEvidence as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
