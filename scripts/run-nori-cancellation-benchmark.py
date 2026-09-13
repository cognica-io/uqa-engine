#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure cooperative Nori cancellation, reservation cleanup, and complete recovery."""

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
BENCHMARK = ROOT / "crates/uqa-analysis/benches/nori.rs"
SUPPORT = ROOT / "crates/uqa-analysis/benches/nori/cancellation.rs"
LIMITS = ROOT / "benchmarks/nori/cancellation-limits.json"
OWNERS = ("uqa-analysis", "uqa-core", "uqa-nori-data")
PROTOCOL = {"samples": 7, "warmup": 2, "batch_operations": 16, "minimum_sample_time_ns": 75_000_000, "allocation_samples": 1, "clock": "per_operation"}
TIMINGS = ("operation_timing", "response_timing")
OUTPUT_KEYS = ("input_bytes", "input_utf16", "input_sha256", "output_sha256", "tokens", "complete_polls", "cancel_at_poll")
POINTS = ("first", "middle", "last")


def target_key(report: dict) -> str:
    return "/".join(str(report.get(key, "")) for key in ("target_os", "target_arch", "pointer_bits"))


def expected_outputs() -> dict:
    reviewed = json.loads(common.LIMITS.read_text())["outputs"]
    outputs = {}
    for case in json.loads(common.CORPUS.read_text())["cases"]:
        text = case["text"] * case["repeat"]
        identity = {"input_bytes": len(text.encode()), "input_utf16": len(text.encode("utf-16-le")) // 2,
                    "input_sha256": hashlib.sha256(text.encode()).hexdigest()}
        for stage in ("tokenizer", "analyzer"):
            for mode in ("None", "Discard", "Mixed"):
                name = f"{stage}/{mode}/{case['name']}"
                outputs[name] = {**identity, **reviewed[name]}
    if set(outputs) != set(reviewed):
        raise RuntimeError("reviewed analysis outputs do not cover the fixed cancellation corpus")
    return outputs


def measurements(report: dict) -> dict:
    if report.get("schema_version") != 2 or report.get("owner") != "uqa-analysis" or report.get("purpose") != "cooperative_cancellation":
        raise RuntimeError("unsupported cancellation measurement schema or owner")
    if report.get("protocol") != PROTOCOL or report.get("threads") != 1:
        raise RuntimeError("invalid cancellation sampling protocol")
    if report.get("pointer_bits") not in (32, 64) or not report.get("target_arch") or not report.get("target_os"):
        raise RuntimeError("invalid cancellation target")
    if report.get("memory_limit_bytes") != 256 * 1024 * 1024 or report.get("unrelated_reservation_bytes") != 4096:
        raise RuntimeError("changed cancellation allowance or unrelated reservation")
    signature = report.get("provenance", {}).get("flags_sha256", "")
    if not isinstance(signature, str) or len(signature) != 64 or any(value not in "0123456789abcdef" for value in signature):
        raise RuntimeError("cancellation measurements require a compiler-flag identity")
    analysis = json.loads(common.LIMITS.read_text())
    for key in ("bundle_bytes", "bundle_sha256", "corpus_sha256"):
        if report.get(key) != analysis[key]:
            raise RuntimeError(f"changed cancellation input identity: {key}")
    if report["corpus_sha256"] != common.digest(common.CORPUS):
        raise RuntimeError("cancellation corpus differs from source")
    if not report.get("timing_scope") or not report.get("allocation_scope"):
        raise RuntimeError("cancellation measurements must record their timing/allocation scope")
    outputs = expected_outputs()
    names = {f"{name}/{point}" for name in outputs for point in POINTS}
    rows = report.get("measurements", [])
    indexed = {row["name"]: row for row in rows}
    if set(indexed) != names or len(rows) != len(names):
        raise RuntimeError("missing, duplicate, or unknown cancellation workload")
    polls = {}
    for name, row in indexed.items():
        base, point = name.rsplit("/", 1)
        if any(row.get(key) != value for key, value in outputs[base].items()):
            raise RuntimeError(f"changed complete recovery output or input: {name}")
        count = row.get("complete_polls")
        if type(count) is not int or count < 3 or polls.setdefault(base, count) != count:
            raise RuntimeError(f"invalid or inconsistent cancellation poll count: {name}")
        expected = {"first": 1, "middle": (count + 1) // 2, "last": count}[point]
        if type(row.get("cancel_at_poll")) is not int or row["cancel_at_poll"] != expected:
            raise RuntimeError(f"changed cancellation point: {name}")
        iterations = row["operation_timing"]["iterations"]
        wall = row.get("sample_wall_ns", [])
        if not isinstance(iterations, list) or len(iterations) != 7 or any(type(value) is not int or value < 16 or value % 16 for value in iterations):
            raise RuntimeError(f"invalid cancellation sample operation counts: {name}")
        if len(wall) != 7 or any(type(value) is not int or value < PROTOCOL["minimum_sample_time_ns"] for value in wall):
            raise RuntimeError(f"insufficient cancellation sample duration: {name}")
        if row.get("verified_cancellations") != 3 + sum(iterations) or row.get("verified_recoveries") != 10 or row.get("remaining_budget_bytes") != 4096:
            raise RuntimeError(f"incomplete cancellation, cleanup, or recovery verification: {name}")
        allocation = row["allocation"]
        if set(allocation) != set(common.ALLOCATION_KEYS) or any(type(value) is not int or value < 0 for value in allocation.values()):
            raise RuntimeError(f"invalid cancellation allocation counters: {name}")
        for unit in ("count", "bytes"):
            if allocation[f"{unit}_retained"] != 0 or not 0 <= allocation[f"{unit}_peak"] <= allocation[f"{unit}_total"]:
                raise RuntimeError(f"leaked or inconsistent cancellation allocation: {name}")
        for key in TIMINGS:
            timing = row[key]
            samples = timing["elapsed_ns"]
            if len(samples) != 7 or any(type(value) is not int or value < 0 for value in samples) or timing.get("iterations") != iterations:
                raise RuntimeError(f"invalid cancellation timing samples: {name}/{key}")
            value = timing.get("median_ns")
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value != statistics.median(elapsed / count for elapsed, count in zip(samples, iterations)):
                raise RuntimeError(f"invalid cancellation timing estimator: {name}/{key}")
        if any(total <= 0 or response > total or total > duration for total, response, duration in zip(row["operation_timing"]["elapsed_ns"], row["response_timing"]["elapsed_ns"], wall)):
            raise RuntimeError(f"inconsistent cancellation return and operation durations: {name}")
    return indexed


def check(report: dict, limits: dict, baseline: dict | None = None) -> dict:
    rows = measurements(report)
    if limits.get("schema_version") != 1:
        raise RuntimeError("unsupported cancellation limit schema")
    for key in ("bundle_bytes", "bundle_sha256", "corpus_sha256"):
        if limits.get(key) != report.get(key):
            raise RuntimeError(f"changed reviewed cancellation identity: {key}")
    ceilings = limits.get("allocation_ceilings", {}).get(target_key(report))
    if ceilings is None or set(ceilings) != set(rows) or set(limits["outputs"]) != set(rows):
        raise RuntimeError("cancellation limits do not cover this complete target")
    for name, row in rows.items():
        output = limits["outputs"][name]
        if set(output) != set(OUTPUT_KEYS) or any(row[key] != value for key, value in output.items()):
            raise RuntimeError(f"changed reviewed cancellation output or poll: {name}")
        if set(ceilings[name]) != set(common.ALLOCATION_KEYS):
            raise RuntimeError(f"incomplete cancellation allocation ceilings: {name}")
        for key, value in row["allocation"].items():
            ceiling = ceilings[name][key]
            if type(ceiling) is not int or ceiling < 0 or value > ceiling:
                raise RuntimeError(f"cancellation allocation regression: {name}/{key}: {value} > {ceiling}")
    maximum = limits["timing_max_ratio"]
    if isinstance(maximum, bool) or not math.isfinite(maximum) or maximum <= 1:
        raise RuntimeError("cancellation timing ceiling must be a finite ratio greater than one")
    ratios = {}
    if baseline is not None:
        before = measurements(baseline)
        for key in ("protocol", "target_arch", "target_os", "pointer_bits", "memory_limit_bytes", "unrelated_reservation_bytes", "timing_scope", "allocation_scope"):
            if report.get(key) != baseline.get(key):
                raise RuntimeError(f"incomparable cancellation measurement: {key}")
        for key in ("cpu", "platform", "rustc", "flags", "flags_sha256", "node", "emcc", "benchmark_sha256", "arguments"):
            if key not in report["provenance"] or report["provenance"][key] != baseline["provenance"].get(key):
                raise RuntimeError(f"incomparable cancellation environment: {key}")
        if report["provenance"]["cpu"] == "unknown":
            raise RuntimeError("cancellation timing comparison requires an identified CPU")
        for name, row in rows.items():
            if any(row[key] != before[name].get(key) for key in OUTPUT_KEYS):
                raise RuntimeError(f"incomparable cancellation input, output, or polling: {name}")
            for key in TIMINGS:
                earlier = before[name][key]["median_ns"]
                current = row[key]["median_ns"]
                if earlier <= 0 or current <= 0:
                    raise RuntimeError(f"clock resolution cannot establish a cancellation timing ratio: {name}/{key}")
                ratio = current / earlier
                ratios[f"{name}/{key}"] = ratio
                if ratio > maximum:
                    raise RuntimeError(f"cancellation timing regression: {name}/{key}: {ratio} > {maximum}")
    return {"allocation_and_recovery_passed": True, "timing_compared": baseline is not None, "timing_ratios": ratios}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("native", "wasm"), default="native")
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--limits", type=pathlib.Path, default=LIMITS)
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--measure-only", action="store_true")
    args = parser.parse_args()
    if args.measure_only and args.baseline:
        parser.error("--baseline requires reviewed gates")
    protected = [args.limits, common.LIMITS, common.CORPUS, BENCHMARK, SUPPORT] + ([args.baseline] if args.baseline else [])
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite reviewed inputs, source, or baseline")
    report = json.loads(args.report.read_text()) if args.report else common.execute_benchmark(
        args.target, "uqa-analysis", "nori", "nori", OWNERS, (SUPPORT,), arguments=("--cancellation",))
    measurements(report)
    report["gate"] = {"allocation_and_recovery_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text()), baseline)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Nori cancellation {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
