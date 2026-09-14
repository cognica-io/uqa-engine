#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure complete Nori graph phrases through the operator owner and check resource/output gates."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import pathlib
import statistics
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("nori_measurement", ROOT / "scripts/run-nori-benchmark.py")
common = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(common)
LIMITS = ROOT / "benchmarks/nori/phrase-limits.json"
BENCHMARK = ROOT / "crates/uqa-operators/benches/nori_phrase.rs"
OWNERS = ("uqa-operators", "uqa-scoring", "uqa-fusion", "uqa-storage", "uqa-analysis", "uqa-core", "uqa-nori-data")
PROTOCOL = {"samples": 7, "warmup": 1, "timed_operations_per_sample": 1}
STAGES = ("match_graph", "analysis_and_match")
MODES = ("None", "Discard", "Mixed")
DOCUMENTS = 2048
ALLOCATION_KEYS = set(common.ALLOCATION_KEYS)
OUTPUT_KEYS = {"query_occurrences", "query_sha256", "analyzer_fingerprint", "rows"}


def cases() -> list:
    return json.loads(common.CORPUS.read_text())["cases"]


def expected_names() -> set:
    return {f"{stage}/{mode}/{case['name']}" for stage in STAGES for mode in MODES for case in cases()}


def same_rows(left: list, right: list) -> bool:
    return len(left) == len(right) and all(
        a[0] == b[0] and math.isclose(a[1], b[1], rel_tol=1e-12, abs_tol=1e-12)
        for a, b in zip(left, right))


def measurements(report: dict) -> dict:
    if report.get("schema_version") != 1 or report.get("owner") != "uqa-operators":
        raise RuntimeError("unsupported phrase measurement schema or owner")
    fixture = cases()
    if report.get("protocol") != PROTOCOL or report.get("threads") != 1 or report.get("pointer_bits") not in (32, 64):
        raise RuntimeError("invalid phrase sampling protocol or target")
    if report.get("documents") != DOCUMENTS + len(fixture) or report.get("memory_limit") != 256 * 1024 * 1024:
        raise RuntimeError("changed phrase fixture size or query allowance")
    if report.get("corpus_sha256") != common.digest(common.CORPUS):
        raise RuntimeError("phrase corpus does not match checked-out source")
    signature = report.get("provenance", {}).get("flags_sha256", "")
    if not isinstance(signature, str) or len(signature) != 64 or any(c not in "0123456789abcdef" for c in signature):
        raise RuntimeError("phrase measurements require a complete compiler-flag identity")
    rows = report.get("measurements", [])
    indexed = {row["name"]: row for row in rows}
    if set(indexed) != expected_names() or len(rows) != len(indexed):
        raise RuntimeError("missing, duplicate, or unknown phrase workload")
    fingerprints = {}
    for name, row in indexed.items():
        _, mode, case_name = name.split("/")
        case_index, case = next((i, case) for i, case in enumerate(fixture) if case["name"] == case_name)
        if row.get("query_sha256") != hashlib.sha256(case["text"].encode()).hexdigest():
            raise RuntimeError(f"changed complete phrase input: {name}")
        if type(row.get("query_occurrences")) is not int or row["query_occurrences"] <= 0:
            raise RuntimeError(f"empty or invalid analyzed phrase: {name}")
        fingerprint = row.get("analyzer_fingerprint", "")
        if not isinstance(fingerprint, str) or len(fingerprint) != 64 or any(c not in "0123456789abcdef" for c in fingerprint):
            raise RuntimeError(f"invalid phrase analyzer identity: {name}")
        if fingerprints.setdefault(mode, fingerprint) != fingerprint:
            raise RuntimeError(f"inconsistent phrase analyzer identity: {mode}")
        samples = row["elapsed_ns"]
        if len(samples) != PROTOCOL["samples"] or any(type(value) is not int or value <= 0 for value in samples):
            raise RuntimeError(f"invalid phrase timing samples: {name}")
        if type(row.get("median_ns")) is not int or row["median_ns"] != statistics.median(samples):
            raise RuntimeError(f"invalid phrase timing estimator: {name}")
        if row.get("verified_samples") != PROTOCOL["samples"] + 2:
            raise RuntimeError(f"missing repeated phrase output verification: {name}")
        allocation = row["allocation"]
        if set(allocation) != ALLOCATION_KEYS or any(type(value) is not int for value in allocation.values()):
            raise RuntimeError(f"invalid phrase allocation counters: {name}")
        for unit in ("count", "bytes"):
            if not 0 <= allocation[f"{unit}_retained"] <= allocation[f"{unit}_peak"] <= allocation[f"{unit}_total"]:
                raise RuntimeError(f"inconsistent phrase allocation counters: {name}")
        last = -1
        control = DOCUMENTS + case_index
        for entry in row["rows"]:
            if len(entry) != 2:
                raise RuntimeError(f"invalid scored phrase row: {name}")
            doc_id, score = entry
            if type(doc_id) is not int or doc_id <= last or not (doc_id == control or 0 <= doc_id < DOCUMENTS and doc_id % len(fixture) == case_index):
                raise RuntimeError(f"wrong, duplicate, or unordered phrase document: {name}")
            if type(score) not in (int, float) or not math.isfinite(score) or score <= 0:
                raise RuntimeError(f"invalid phrase score: {name}")
            last = doc_id
        if last != control:
            raise RuntimeError(f"phrase failed to match its complete source control: {name}")
        if len(row["rows"]) != len(range(case_index, DOCUMENTS, len(fixture))) + 1:
            raise RuntimeError(f"phrase omitted a matching repeated document: {name}")
    if len(set(fingerprints.values())) != len(MODES):
        raise RuntimeError("phrase modes must retain distinct analyzer identities")
    for mode in MODES:
        for case in fixture:
            first, second = (indexed[f"{stage}/{mode}/{case['name']}"] for stage in STAGES)
            if any(first[key] != second[key] for key in OUTPUT_KEYS):
                raise RuntimeError("pre-analyzed and complete phrase results differ")
    return indexed


def check(report: dict, limits: dict, baseline: dict | None = None) -> dict:
    rows = measurements(report)
    if limits.get("schema_version") != 1 or limits.get("corpus_sha256") != report["corpus_sha256"]:
        raise RuntimeError("unsupported phrase limit schema or corpus")
    ceilings = limits["allocation_ceilings"][str(report["pointer_bits"])]
    if set(ceilings) != set(rows) or set(limits["outputs"]) != set(rows):
        raise RuntimeError("phrase limits do not cover every workload")
    for name, row in rows.items():
        expected = limits["outputs"][name]
        if set(expected) != OUTPUT_KEYS or any(row[key] != expected[key] for key in OUTPUT_KEYS - {"rows"}) or not same_rows(row["rows"], expected["rows"]):
            raise RuntimeError(f"phrase analyzer, graph size, document support, or scores changed: {name}")
        if set(ceilings[name]) != ALLOCATION_KEYS:
            raise RuntimeError(f"incomplete phrase allocation ceiling: {name}")
        for key, value in row["allocation"].items():
            ceiling = ceilings[name][key]
            if type(ceiling) is not int or ceiling < 0:
                raise RuntimeError(f"invalid phrase allocation ceiling: {name}/{key}")
            if value > ceiling:
                raise RuntimeError(f"phrase allocation regression: {name}/{key}: {value} > {ceiling}")
    maximum = limits["timing_max_ratio"]
    if isinstance(maximum, bool) or not math.isfinite(maximum) or maximum <= 1:
        raise RuntimeError("phrase timing ceiling must be finite and greater than one")
    ratios = {}
    if baseline is not None:
        before = measurements(baseline)
        for key in ("protocol", "pointer_bits", "target_arch", "target_os", "corpus_sha256", "documents", "memory_limit", "timing_scope", "allocation_scope"):
            if report.get(key) != baseline.get(key):
                raise RuntimeError(f"incomparable phrase timing scope: {key}")
        for key in ("cpu", "platform", "rustc", "flags", "flags_sha256", "node", "emcc", "benchmark_sha256"):
            if key not in report["provenance"] or key not in baseline["provenance"] or report["provenance"][key] != baseline["provenance"][key]:
                raise RuntimeError(f"incomparable phrase timing environment: {key}")
        if report["provenance"]["cpu"] == "unknown":
            raise RuntimeError("phrase timing comparison requires an identified CPU")
        for name, row in rows.items():
            if any(row[key] != before[name][key] for key in OUTPUT_KEYS - {"rows"}) or not same_rows(row["rows"], before[name]["rows"]):
                raise RuntimeError(f"incomparable phrase timing output: {name}")
            ratios[name] = row["median_ns"] / before[name]["median_ns"]
            if ratios[name] > maximum:
                raise RuntimeError(f"phrase timing regression: {name}: {ratios[name]:.3f} > {maximum}")
    return {"allocation_and_rows_passed": True, "timing_compared": baseline is not None, "timing_ratios": ratios}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("native", "wasm"), default="native")
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--limits", type=pathlib.Path, default=LIMITS)
    parser.add_argument("--measure-only", action="store_true")
    parser.add_argument("--baseline", type=pathlib.Path)
    args = parser.parse_args()
    if args.measure_only and args.baseline:
        parser.error("--baseline requires reviewed gates")
    protected = [args.limits, common.CORPUS, BENCHMARK] + ([args.baseline] if args.baseline else [])
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite reviewed limits, corpus, benchmark source, or baseline")
    report = json.loads(args.report.read_text()) if args.report else common.execute_benchmark(
        args.target, "uqa-operators", "nori_phrase", "uqa-analysis/nori", OWNERS)
    measurements(report)
    report["gate"] = {"allocation_and_rows_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text()), baseline)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Nori phrase {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
