#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure Nori index transactions in each physical provider with reopen verification."""

from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("nori_index_measurement", ROOT / "scripts/run-nori-index-benchmark.py")
index = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(index)
common = index.common
SUPPORT = ROOT / "benchmarks/nori/persistent.rs"
LIMITS = ROOT / "benchmarks/nori/persistent-limits.json"
EXPECTED = {"commit_batch_256/0": 256, "commit_batch_16/256": 272,
            "commit_batch_16/2048": 2064, "rollback_batch_16/2048": 2048}
TRANSACTION_MODELS = {"sqlite": "provider_serialized", "redb": "versioned_concurrent"}


def target_key(report: dict) -> str:
    return "/".join(str(report.get(key, "")) for key in ("target_os", "target_arch", "pointer_bits"))


def target_limits(report: dict, limits: dict) -> dict:
    if limits.get("schema_version") != 2:
        raise RuntimeError("persistent allocation limits require target-specific calibration")
    target = target_key(report)
    ceilings = limits.get("allocation_ceilings", {}).get(target)
    if ceilings is None:
        raise RuntimeError(f"no reviewed persistent allocation limits for target: {target}")
    return {**limits, "schema_version": 1, "allocation_ceilings": {str(report["pointer_bits"]): ceilings}}


def measurements(report: dict, provider: str) -> dict:
    allocation_only = report.get("protocol") == index.ALLOCATION_PROTOCOL
    rows = index.measurements(report, EXPECTED, f"uqa-storage-{provider}", allocation_only=allocation_only)
    signature = report.get("provenance", {}).get("flags_sha256", "")
    if not isinstance(signature, str) or len(signature) != 64 or any(c not in "0123456789abcdef" for c in signature):
        raise RuntimeError("persistent measurements require a complete compiler-flag identity")
    if provider == "redb" and report.get("target_os") == "emscripten":
        raise RuntimeError("redb does not provide an Emscripten persistence measurement")
    model = TRANSACTION_MODELS[provider]
    if report.get("transaction_model") != model:
        raise RuntimeError(f"persistent measurements require the provider's transaction model: {model}")
    for name, row in rows.items():
        expected_samples = 1 if allocation_only else index.PROTOCOL["samples"] + 2
        if type(row.get("verified_live_and_reopened_samples")) is not int or row["verified_live_and_reopened_samples"] != expected_samples:
            raise RuntimeError(f"missing live/reopened verification for every sample: {name}")
        for key in ("closed_seed_file_bytes", "closed_result_file_bytes"):
            if type(row.get(key)) is not int or row[key] <= 0:
                raise RuntimeError(f"invalid persistent file-size observation: {name}/{key}")
        if "retained_transaction_bytes" not in row:
            raise RuntimeError(f"missing transaction retention observation: {name}")
        retained = row["retained_transaction_bytes"]
        if model == "versioned_concurrent":
            if type(retained) is not int or retained != 0:
                raise RuntimeError(f"private transaction retention must return to zero: {name}")
        elif retained is not None:
            raise RuntimeError(f"serialized transactions do not report a private MVCC allowance: {name}")
    if not report.get("durability") or not report.get("filesystem"):
        raise RuntimeError("persistent measurements require durability and filesystem scope")
    return rows


def check(report: dict, limits: dict, provider: str, baseline: dict | None = None) -> dict:
    measurements(report, provider)
    allocation_only = report["protocol"] == index.ALLOCATION_PROTOCOL
    if baseline is not None and (allocation_only or baseline.get("protocol") == index.ALLOCATION_PROTOCOL):
        raise RuntimeError("allocation-only verification cannot compare timing baselines")
    if baseline is not None:
        measurements(baseline, provider)
        for key in ("durability", "filesystem"):
            if report[key] != baseline[key]:
                raise RuntimeError(f"incomparable persistent timing scope: {key}")
    return index.check(report, target_limits(report, limits), baseline, expected=EXPECTED, owner=f"uqa-storage-{provider}", allocation_only=allocation_only)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=("sqlite", "redb"), required=True)
    parser.add_argument("--target", choices=("native", "wasm"), default="native")
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--limits", type=pathlib.Path, default=LIMITS)
    parser.add_argument("--measure-only", action="store_true")
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--allocation-only", action="store_true", help="verify one allocation/output sample per workload without warmups or timing")
    args = parser.parse_args()
    if args.provider == "redb" and args.target == "wasm":
        parser.error("redb measurements require a native target")
    if args.measure_only and args.baseline:
        parser.error("--baseline requires reviewed gates")
    if args.allocation_only and args.baseline:
        parser.error("--allocation-only cannot compare timing baselines")
    protected = [args.limits, common.CORPUS, SUPPORT] + ([args.baseline] if args.baseline else [])
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite reviewed limits, corpus, benchmark source, or baseline")
    owner = f"uqa-storage-{args.provider}"
    owners = (owner, *index.OWNERS) + (("uqa-graph", "uqa-operators", "uqa-scoring", "uqa-fusion") if args.provider == "sqlite" else ())
    report = json.loads(args.report.read_text()) if args.report else common.execute_benchmark(
        args.target, owner, "nori_persistent", "uqa-analysis/nori", owners, (SUPPORT,),
        arguments=("--allocation-only",) if args.allocation_only else ())
    measurements(report, args.provider)
    if args.allocation_only and report["protocol"] != index.ALLOCATION_PROTOCOL:
        parser.error("--allocation-only requires an allocation-only report")
    report["gate"] = {"allocation_and_graph_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text())[args.provider], args.provider, baseline)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Nori {args.provider} {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
