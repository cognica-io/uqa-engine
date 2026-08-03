#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Build a provenance report and enforce same-host analytical ratios."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import pathlib
import platform
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
]


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
    parser.add_argument("--criterion-root", type=pathlib.Path, default=ROOT / "target/criterion")
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=ROOT / "target/benchmark-runs/analytical-comparison.json",
    )
    args = parser.parse_args()

    manifest_bytes = MANIFEST_PATH.read_bytes()
    manifest = json.loads(manifest_bytes)
    criterion_config = manifest.get("criterion", {})
    if criterion_config.get("sampling_mode") != "linear":
        raise RuntimeError("analytical ratio gates require Criterion linear sampling")
    if criterion_config.get("point_estimator") != "slope":
        raise RuntimeError("analytical ratio gates require the Criterion slope estimator")

    estimates: dict[str, float] = {}
    ratios: list[dict[str, object]] = []
    failed = False

    def load_estimate(name: str) -> float:
        if name not in estimates:
            estimates[name] = slope_estimate(args.criterion_root, name)
        return estimates[name]

    for gate in manifest["ratio_gates"]:
        numerator_name = gate["numerator"]
        denominator_name = gate["denominator"]
        numerator = load_estimate(numerator_name)
        denominator = load_estimate(denominator_name)
        ratio = numerator / denominator
        passed = ratio <= float(gate["max"])
        failed |= not passed
        ratios.append(
            {
                "name": gate["name"],
                "numerator": numerator_name,
                "denominator": denominator_name,
                "ratio": ratio,
                "maximum": gate["max"],
                "passed": passed,
            }
        )

    status = command("git", "status", "--porcelain")
    report = {
        "schema_version": 2,
        "generated_at_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        "manifest": str(MANIFEST_PATH.relative_to(ROOT)),
        "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "benchmark_source_sha256": source_hash(),
        "git_commit": git_value("rev-parse", "HEAD"),
        "git_dirty": bool(status),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor() or "unknown",
        "rustc": command("rustc", "--version"),
        "cargo": command("cargo", "--version"),
        "criterion_slope_nanoseconds_per_iteration": dict(sorted(estimates.items())),
        "ratio_gates": ratios,
        "passed": not failed,
        "independent_reproduction": False,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    for gate in ratios:
        marker = "PASS" if gate["passed"] else "FAIL"
        print(
            f"{marker} {gate['name']}: {gate['ratio']:.3f} <= {gate['maximum']:.3f}"
        )
    print(f"Benchmark provenance report: {args.output}")
    return int(failed)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(2) from error
