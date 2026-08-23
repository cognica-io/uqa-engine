#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Build analytical provenance and enforce paired base/head regressions."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import statistics
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "benchmarks" / "analytical" / "manifest.json"
BENCHMARK_SOURCES = [
    MANIFEST_PATH,
    ROOT / "Cargo.lock",
    ROOT / "crates/uqa-engine/benches/analytical_comparison.rs",
    ROOT / "crates/uqa-engine/benches/analytical_comparison/backends.rs",
    ROOT / "crates/uqa-engine/benches/analytical_comparison/fixture.rs",
    ROOT / "scripts/check-analytical-benchmark.py",
    ROOT / "scripts/run-analytical-comparison.sh",
    ROOT / "scripts/run-analytical-regression.py",
]
WORKLOAD_IDENTITY_KEYS = (
    "workload",
    "rows",
    "seed",
    "work_mem",
    "generator",
    "schema_sql",
    "queries",
)


def command(*args: str) -> str:
    return subprocess.run(
        args,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def slope_estimate(criterion_root: pathlib.Path, benchmark: str) -> float:
    path = criterion_root.joinpath(*benchmark.split("/"), "new", "estimates.json")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
        value = float(payload["slope"]["point_estimate"])
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise RuntimeError(f"read Criterion linear-sampling slope {path}: {error}") from error
    if not value > 0:
        raise RuntimeError(f"Criterion slope must be positive: {path}: {value}")
    return value


def workload_identity(manifest: dict[str, object]) -> dict[str, object]:
    return {key: manifest.get(key) for key in WORKLOAD_IDENTITY_KEYS}


def object_hash(value: object) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def file_hash(path: pathlib.Path | None) -> str | None:
    if path is None:
        return None
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cpu_model() -> str:
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    if sys.platform == "darwin":
        try:
            return command("sysctl", "-n", "machdep.cpu.brand_string")
        except subprocess.CalledProcessError:
            pass
    return platform.processor() or "unknown"


def git_value(*args: str) -> str:
    try:
        return command("git", *args)
    except subprocess.CalledProcessError:
        return "unknown"


def source_hash() -> str:
    digest = hashlib.sha256()
    for path in sorted(BENCHMARK_SOURCES):
        digest.update(str(path.relative_to(ROOT)).encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--criterion-root", type=pathlib.Path, action="append")
    parser.add_argument("--baseline-criterion-root", type=pathlib.Path, action="append")
    parser.add_argument("--baseline-manifest", type=pathlib.Path)
    parser.add_argument("--baseline-revision")
    parser.add_argument("--head-executable", type=pathlib.Path)
    parser.add_argument("--baseline-executable", type=pathlib.Path)
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=ROOT / "target/benchmark-runs/analytical-comparison.json",
    )
    args = parser.parse_args()
    criterion_roots = args.criterion_root or [ROOT / "target/criterion"]
    baseline_roots = args.baseline_criterion_root or []
    if baseline_roots and len(baseline_roots) != len(criterion_roots):
        raise RuntimeError("base and head Criterion roots must form equal-sized pairs")
    if args.baseline_manifest and not baseline_roots:
        raise RuntimeError("--baseline-manifest requires paired baseline roots")

    manifest_bytes = MANIFEST_PATH.read_bytes()
    manifest = json.loads(manifest_bytes)
    criterion_config = manifest.get("criterion", {})
    if criterion_config.get("sampling_mode") != "linear":
        raise RuntimeError("analytical ratio gates require Criterion linear sampling")
    if criterion_config.get("point_estimator") != "slope":
        raise RuntimeError("analytical ratio gates require the Criterion slope estimator")

    head_samples: dict[str, list[float]] = {}
    base_samples: dict[str, list[float]] = {}
    external_ratios: list[dict[str, object]] = []
    regression_ratios: list[dict[str, object]] = []
    external_failed = False
    regression_failed = False

    def load_samples(
        cache: dict[str, list[float]], roots: list[pathlib.Path], name: str
    ) -> list[float]:
        if name not in cache:
            cache[name] = [slope_estimate(root, name) for root in roots]
        return cache[name]

    for gate in manifest["external_ratio_checks"]:
        numerator_name = gate["numerator"]
        denominator_name = gate["denominator"]
        numerators = load_samples(head_samples, criterion_roots, numerator_name)
        denominators = load_samples(head_samples, criterion_roots, denominator_name)
        paired = [left / right for left, right in zip(numerators, denominators)]
        ratio = statistics.median(paired)
        passed = ratio <= float(gate["max"])
        external_failed |= not passed
        external_ratios.append(
            {
                "name": gate["name"],
                "numerator": numerator_name,
                "denominator": denominator_name,
                "paired_ratios": paired,
                "ratio": ratio,
                "maximum": gate["max"],
                "passed": passed,
            }
        )

    regression_protocol = manifest.get("regression_protocol", {})
    expected_pairs = int(regression_protocol.get("pairs", 0))
    if baseline_roots:
        if expected_pairs < 2 or expected_pairs % 2:
            raise RuntimeError("analytical regression requires an even pair count of at least two")
        if len(criterion_roots) != expected_pairs:
            raise RuntimeError(
                f"analytical regression requires {expected_pairs} paired Criterion roots"
            )
        if regression_protocol.get("ordering") != "counterbalanced":
            raise RuntimeError("analytical regression requires counterbalanced execution")
        if (
            regression_protocol.get("point_estimator")
            != "median_of_paired_slope_ratios"
        ):
            raise RuntimeError("analytical regression estimator contract changed")

    for gate in manifest.get("regression_gates", []):
        if not baseline_roots:
            break
        benchmark = gate["benchmark"]
        heads = load_samples(head_samples, criterion_roots, benchmark)
        bases = load_samples(base_samples, baseline_roots, benchmark)
        paired = [head / base for head, base in zip(heads, bases)]
        ratio = statistics.median(paired)
        passed = ratio <= float(gate["max"])
        regression_failed |= not passed
        regression_ratios.append(
            {
                "name": gate["name"],
                "benchmark": benchmark,
                "paired_ratios": paired,
                "ratio": ratio,
                "maximum": gate["max"],
                "passed": passed,
            }
        )

    head_identity = workload_identity(manifest)
    baseline_identity_hash = None
    if args.baseline_manifest:
        baseline_manifest = json.loads(args.baseline_manifest.read_text(encoding="utf-8"))
        baseline_identity = workload_identity(baseline_manifest)
        if baseline_identity != head_identity:
            raise RuntimeError("base and head analytical workload identities differ")
        baseline_identity_hash = object_hash(baseline_identity)

    external_enforced = not baseline_roots
    failed = regression_failed or (external_enforced and external_failed)
    head_estimates = {
        name: statistics.median(samples) for name, samples in sorted(head_samples.items())
    }
    base_estimates = {
        name: statistics.median(samples) for name, samples in sorted(base_samples.items())
    }

    status = command("git", "status", "--porcelain")
    report = {
        "schema_version": 3,
        "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "manifest": str(MANIFEST_PATH.relative_to(ROOT)),
        "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "workload_identity_sha256": object_hash(head_identity),
        "benchmark_source_sha256": source_hash(),
        "benchmark_executable_sha256": file_hash(args.head_executable),
        "git_commit": git_value("rev-parse", "HEAD"),
        "git_dirty": bool(status),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor() or "unknown",
        "cpu_model": cpu_model(),
        "logical_cpu_count": os.cpu_count(),
        "rustc": command("rustc", "--version"),
        "cargo": command("cargo", "--version"),
        "criterion_slope_nanoseconds_per_iteration": head_estimates,
        "criterion_slope_samples_nanoseconds_per_iteration": dict(
            sorted(head_samples.items())
        ),
        "baseline": {
            "git_commit": args.baseline_revision or "unknown",
            "manifest": str(args.baseline_manifest) if args.baseline_manifest else None,
            "workload_identity_sha256": baseline_identity_hash,
            "benchmark_executable_sha256": file_hash(args.baseline_executable),
            "criterion_slope_nanoseconds_per_iteration": base_estimates,
            "criterion_slope_samples_nanoseconds_per_iteration": dict(
                sorted(base_samples.items())
            ),
        }
        if baseline_roots
        else None,
        "external_ratio_checks": external_ratios,
        "external_ratios_enforced": external_enforced,
        "regression_gates": regression_ratios,
        "passed": not failed,
        "independent_reproduction": False,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    for gate in external_ratios:
        marker = "PASS" if gate["passed"] else ("FAIL" if external_enforced else "WARN")
        print(
            f"{marker} external {gate['name']}: "
            f"{gate['ratio']:.3f} <= {gate['maximum']:.3f}"
        )
    for gate in regression_ratios:
        marker = "PASS" if gate["passed"] else "FAIL"
        print(
            f"{marker} regression {gate['name']}: "
            f"{gate['ratio']:.3f} <= {gate['maximum']:.3f}"
        )
    print(f"Benchmark provenance report: {args.output}")
    return int(failed)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(2) from error
