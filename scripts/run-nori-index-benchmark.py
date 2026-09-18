#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure native graph indexing through its storage owner and enforce allocation ceilings."""

from __future__ import annotations

import argparse
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
LIMITS = ROOT / "benchmarks/nori/index-limits.json"
EXPECTED = {"build_points/256": 256, "build_points/2048": 2048,
            "append_batch_16/0": 16, "append_batch_16/256": 272, "append_batch_16/2048": 2064}
ALLOCATION_KEYS = {"count_total", "count_peak", "count_net", "bytes_total", "bytes_peak", "bytes_net"}
PROTOCOL = {"samples": 7, "warmup": 1, "timed_operations_per_sample": 1}
ALLOCATION_PROTOCOL = {"allocation_samples": 1, "samples": 0, "warmup": 0, "timed_operations_per_sample": 0}
OWNERS = ("uqa-storage", "uqa-analysis", "uqa-core", "uqa-nori-data")


def measurements(report: dict, expected: dict = EXPECTED, owner: str = "uqa-storage", *, allocation_only: bool = False) -> dict:
    if report.get("schema_version") != 1 or report.get("owner") != owner:
        raise RuntimeError("unsupported Nori indexing benchmark schema or owner")
    protocol = ALLOCATION_PROTOCOL if allocation_only else PROTOCOL
    sampling = report.get("protocol")
    if sampling != protocol or any(type(value) is not int for value in sampling.values()) or type(report.get("threads")) is not int or report["threads"] != 1 or report.get("pointer_bits") not in (32, 64):
        raise RuntimeError("invalid indexing benchmark sampling protocol or target")
    if allocation_only and "timing_scope" in report:
        raise RuntimeError("allocation-only verification cannot contain timing observations")
    if report.get("corpus_sha256") != common.digest(common.CORPUS):
        raise RuntimeError("index benchmark corpus does not match the checked-out source")
    rows = report.get("measurements", [])
    indexed = {row["name"]: row for row in rows}
    if set(indexed) != set(expected) or len(rows) != len(expected):
        raise RuntimeError("missing, duplicate, or unknown indexing workload")
    for name, row in indexed.items():
        if row.get("documents_after") != expected[name]:
            raise RuntimeError(f"changed indexing document count: {name}")
        if allocation_only:
            if "elapsed_ns" in row or "median_ns" in row:
                raise RuntimeError("allocation-only verification cannot contain timing observations")
        else:
            samples = row["elapsed_ns"]
            if len(samples) != PROTOCOL["samples"] or any(type(sample) is not int or sample <= 0 for sample in samples):
                raise RuntimeError(f"invalid indexing timing samples: {name}")
            if type(row["median_ns"]) is not int or statistics.median(samples) != row["median_ns"]:
                raise RuntimeError(f"invalid indexing timing estimator: {name}")
        allocation = row["allocation"]
        if set(allocation) != ALLOCATION_KEYS or any(type(value) is not int for value in allocation.values()):
            raise RuntimeError(f"invalid indexing allocation counters: {name}")
        for unit in ("count", "bytes"):
            if not 0 <= allocation[f"{unit}_peak"] <= allocation[f"{unit}_total"] or allocation[f"{unit}_net"] > allocation[f"{unit}_peak"]:
                raise RuntimeError(f"inconsistent indexing allocation counters: {name}")
    return indexed


def check(report: dict, limits: dict, baseline: dict | None = None, *, expected: dict = EXPECTED, owner: str = "uqa-storage", allocation_only: bool = False) -> dict:
    if allocation_only and baseline is not None:
        raise RuntimeError("allocation-only verification cannot compare timing baselines")
    rows = measurements(report, expected, owner, allocation_only=allocation_only)
    if limits.get("schema_version") != 1:
        raise RuntimeError("unsupported indexing limit schema")
    for key in ("corpus_sha256", "analyzer_fingerprint"):
        if report.get(key) != limits.get(key):
            raise RuntimeError(f"indexing resource identity changed: {key}")
    ceilings = limits["allocation_ceilings"][str(report["pointer_bits"])]
    if set(ceilings) != set(expected) or set(limits["outputs"]) != set(expected):
        raise RuntimeError("indexing limits do not cover every workload")
    for name, row in rows.items():
        output = limits["outputs"][name]
        if not isinstance(output, dict) or set(output) != {"graph_sha256", "field_length", "posting_count"}:
            raise RuntimeError(f"incomplete indexing output contract: {name}")
        if any(row.get(key) != value for key, value in output.items()):
            raise RuntimeError(f"indexing graph, metadata, or statistics changed: {name}")
        if set(ceilings[name]) != ALLOCATION_KEYS:
            raise RuntimeError(f"incomplete indexing allocation ceilings: {name}")
        for key, value in row["allocation"].items():
            ceiling = ceilings[name][key]
            if type(ceiling) is not int or (not key.endswith("_net") and ceiling < 0):
                raise RuntimeError(f"invalid indexing allocation ceiling: {name}/{key}")
            if value > ceiling:
                raise RuntimeError(f"indexing allocation regression: {name}/{key}: {value} > {ceiling}")
    maximum = limits["timing_max_ratio"]
    if isinstance(maximum, bool) or not math.isfinite(maximum) or maximum <= 1:
        raise RuntimeError("indexing timing ceiling must be a finite ratio greater than one")
    ratios = {}
    if baseline is not None:
        before = measurements(baseline, expected, owner)
        for key in ("protocol", "pointer_bits", "target_arch", "target_os", "corpus_sha256", "analyzer_fingerprint", "timing_scope", "allocation_scope"):
            if report.get(key) != baseline.get(key):
                raise RuntimeError(f"incomparable indexing timing baseline: {key}")
        for key in ("cpu", "platform", "rustc", "flags", "node", "emcc", "benchmark_sha256"):
            if key not in report["provenance"] or key not in baseline["provenance"] or report["provenance"][key] != baseline["provenance"][key]:
                raise RuntimeError(f"incomparable indexing timing environment: {key}")
        if report["provenance"].get("flags_sha256") != baseline["provenance"].get("flags_sha256"):
            raise RuntimeError("incomparable indexing timing environment: flags_sha256")
        if report["provenance"]["cpu"] == "unknown":
            raise RuntimeError("indexing timing comparison requires an identified CPU")
        for name, row in rows.items():
            if any(row[key] != before[name].get(key) for key in ("graph_sha256", "field_length", "posting_count")):
                raise RuntimeError(f"incomparable indexing timing outputs: {name}")
            ratios[name] = row["median_ns"] / before[name]["median_ns"]
            if ratios[name] > maximum:
                raise RuntimeError(f"indexing timing regression: {name}: {ratios[name]:.3f} > {maximum}")
    return {"allocation_and_graph_passed": True, "timing_compared": baseline is not None, "timing_ratios": ratios}


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
    protected = [args.limits, common.CORPUS] + ([args.baseline] if args.baseline else [])
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite reviewed limits, the corpus, or timing baseline")
    report = json.loads(args.report.read_text()) if args.report else common.execute_benchmark(
        args.target, "uqa-storage", "nori_storage", "uqa-analysis/nori", OWNERS)
    measurements(report)
    report["gate"] = {"allocation_and_graph_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text()), baseline)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Nori indexing {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
