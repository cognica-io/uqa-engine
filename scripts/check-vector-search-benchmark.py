#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate vector-search quality and combine it with Criterion measurements."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import pathlib
import platform
import subprocess
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "benchmarks" / "vector-search" / "manifest.json"
DEFAULT_OBSERVATIONS = (
    ROOT / "target" / "benchmark-runs" / "vector-search-observations-standard.json"
)
DEFAULT_OUTPUT = ROOT / "target" / "benchmark-runs" / "vector-search-standard.json"
SCORE_TOLERANCE = 1.0e-6
REQUIRED_STORAGE = {
    "backend": "sqlite",
    "persistent": True,
    "reopened_before_each_query_phase": True,
}
REQUIRED_SQL_API = "Engine::sql"


class BenchmarkError(RuntimeError):
    """A malformed or failed vector-search benchmark report."""


def load_json(path: pathlib.Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(payload, dict):
        raise BenchmarkError(f"JSON root must be an object: {path}")
    return payload


def finite_number(value: object, context: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BenchmarkError(f"{context} must be numeric")
    number = float(value)
    if not math.isfinite(number):
        raise BenchmarkError(f"{context} must be finite")
    return number


def parse_ranked_results(
    algorithm: dict[str, Any], query_count: int, top_k: int
) -> dict[int, list[tuple[int, float]]]:
    name = algorithm.get("name")
    raw_results = algorithm.get("results")
    if not isinstance(name, str) or not isinstance(raw_results, list):
        raise BenchmarkError("algorithm observations require a name and results array")
    parsed: dict[int, list[tuple[int, float]]] = {}
    for raw_query in raw_results:
        if not isinstance(raw_query, dict):
            raise BenchmarkError(f"{name} query result must be an object")
        query_id = raw_query.get("query_id")
        hits = raw_query.get("hits")
        if isinstance(query_id, bool) or not isinstance(query_id, int):
            raise BenchmarkError(f"{name} query_id must be an integer")
        if query_id in parsed:
            raise BenchmarkError(f"{name} repeats query_id {query_id}")
        if not isinstance(hits, list) or len(hits) != top_k:
            raise BenchmarkError(f"{name} query {query_id} must contain exactly {top_k} hits")
        ranked: list[tuple[int, float]] = []
        seen: set[int] = set()
        for rank, hit in enumerate(hits, start=1):
            if not isinstance(hit, dict):
                raise BenchmarkError(f"{name} query {query_id} rank {rank} is not an object")
            doc_id = hit.get("doc_id")
            if isinstance(doc_id, bool) or not isinstance(doc_id, int) or doc_id < 0:
                raise BenchmarkError(f"{name} query {query_id} rank {rank} has invalid doc_id")
            if doc_id in seen:
                raise BenchmarkError(f"{name} query {query_id} repeats doc_id {doc_id}")
            score = finite_number(hit.get("score"), f"{name} query {query_id} rank {rank} score")
            if not -1.0 - SCORE_TOLERANCE <= score <= 1.0 + SCORE_TOLERANCE:
                raise BenchmarkError(f"{name} query {query_id} rank {rank} has invalid cosine score")
            seen.add(doc_id)
            ranked.append((doc_id, score))
        if ranked != sorted(ranked, key=lambda hit: (-hit[1], hit[0])):
            raise BenchmarkError(f"{name} query {query_id} hits are not in deterministic rank order")
        parsed[query_id] = ranked
    expected_ids = set(range(query_count))
    if set(parsed) != expected_ids:
        raise BenchmarkError(f"{name} query IDs differ from 0..{query_count - 1}")
    return parsed


def compute_quality_metrics(
    exact: dict[int, list[tuple[int, float]]],
    candidate: dict[int, list[tuple[int, float]]],
    top_k: int,
) -> dict[str, float]:
    if set(exact) != set(candidate) or not exact:
        raise BenchmarkError("exact and candidate query sets must be identical and non-empty")
    overlap_count = 0
    top_1_matches = 0
    reciprocal_rank_total = 0.0
    exact_set_matches = 0
    returned_count = 0
    top_1_loss_total = 0.0
    shared_score_errors: list[float] = []
    for query_id in sorted(exact):
        exact_hits = exact[query_id]
        candidate_hits = candidate[query_id]
        if len(exact_hits) != top_k or len(candidate_hits) != top_k:
            raise BenchmarkError(f"query {query_id} does not contain exactly k results")
        exact_scores = dict(exact_hits)
        exact_ids = set(exact_scores)
        candidate_ids = [doc_id for doc_id, _ in candidate_hits]
        candidate_set = set(candidate_ids)
        shared = exact_ids & candidate_set
        overlap_count += len(shared)
        returned_count += len(candidate_hits)
        top_1_matches += int(exact_hits[0][0] == candidate_hits[0][0])
        exact_set_matches += int(exact_ids == candidate_set)
        if exact_hits[0][0] in candidate_set:
            reciprocal_rank_total += 1.0 / (candidate_ids.index(exact_hits[0][0]) + 1)
        loss = exact_hits[0][1] - candidate_hits[0][1]
        if loss < -SCORE_TOLERANCE:
            raise BenchmarkError(
                f"candidate query {query_id} exceeds the exact best score by {-loss}"
            )
        top_1_loss_total += max(0.0, loss)
        candidate_scores = dict(candidate_hits)
        shared_score_errors.extend(
            abs(exact_scores[doc_id] - candidate_scores[doc_id]) for doc_id in shared
        )
    query_count = len(exact)
    expected_count = query_count * top_k
    return {
        "recall_at_k": overlap_count / expected_count,
        "top_1_accuracy": top_1_matches / query_count,
        "mrr_at_k": reciprocal_rank_total / query_count,
        "exact_set_rate": exact_set_matches / query_count,
        "result_count_rate": returned_count / expected_count,
        "mean_top_1_similarity_loss": top_1_loss_total / query_count,
        "max_shared_score_abs_error": max(shared_score_errors, default=0.0),
    }


def check_quality_gates(
    algorithm: dict[str, Any], metrics: dict[str, float]
) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []
    name = algorithm["name"]
    for metric, raw_minimum in algorithm.get("minimum_quality", {}).items():
        if metric not in metrics:
            raise BenchmarkError(f"{name} minimum gate names unknown metric {metric}")
        minimum = finite_number(raw_minimum, f"{name} {metric} minimum")
        actual = metrics[metric]
        passed = actual + 1.0e-12 >= minimum
        checks.append(
            {"metric": metric, "relation": ">=", "limit": minimum, "actual": actual, "passed": passed}
        )
    for metric, raw_maximum in algorithm.get("maximum_quality", {}).items():
        if metric not in metrics:
            raise BenchmarkError(f"{name} maximum gate names unknown metric {metric}")
        maximum = finite_number(raw_maximum, f"{name} {metric} maximum")
        actual = metrics[metric]
        passed = actual <= maximum + 1.0e-12
        checks.append(
            {"metric": metric, "relation": "<=", "limit": maximum, "actual": actual, "passed": passed}
        )
    return checks


def criterion_estimate(
    criterion_root: pathlib.Path, benchmark: str, estimator: str
) -> float:
    estimates = criterion_root.joinpath(*benchmark.split("/"), "new", "estimates.json")
    payload = load_json(estimates)
    try:
        point_estimate = payload[estimator]["point_estimate"]
    except (KeyError, TypeError) as error:
        raise BenchmarkError(
            f"missing Criterion {estimator} estimate in {estimates}"
        ) from error
    estimate = finite_number(
        point_estimate, f"Criterion {estimator} estimate for {benchmark}"
    )
    if estimate <= 0.0:
        raise BenchmarkError(
            f"Criterion {estimator} estimate for {benchmark} must be positive"
        )
    return estimate


def git_value(*args: str) -> str:
    process = subprocess.run(
        ["git", *args], cwd=ROOT, check=False, capture_output=True, text=True
    )
    return process.stdout.strip() if process.returncode == 0 else "unknown"


def command_value(*args: str) -> str:
    process = subprocess.run(
        list(args), cwd=ROOT, check=False, capture_output=True, text=True
    )
    return process.stdout.strip() if process.returncode == 0 else "unknown"


def algorithms_by_name(raw: object, context: str) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, list) or not raw:
        raise BenchmarkError(f"{context} algorithms must be a non-empty array")
    algorithms: dict[str, dict[str, Any]] = {}
    for algorithm in raw:
        if not isinstance(algorithm, dict):
            raise BenchmarkError(f"{context} algorithm must be an object")
        name = algorithm.get("name")
        if not isinstance(name, str) or not name or name in algorithms:
            raise BenchmarkError(f"{context} algorithm names must be unique non-empty strings")
        algorithms[name] = algorithm
    return algorithms


def profiles_by_name(raw: object) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, list) or not raw:
        raise BenchmarkError("manifest profiles must be a non-empty array")
    profiles: dict[str, dict[str, Any]] = {}
    for profile in raw:
        if not isinstance(profile, dict):
            raise BenchmarkError("manifest profile must be an object")
        name = profile.get("name")
        if not isinstance(name, str) or not name or name in profiles:
            raise BenchmarkError("manifest profile names must be unique non-empty strings")
        profiles[name] = profile
    return profiles


def positive_integer(value: object, context: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise BenchmarkError(f"{context} must be a positive integer")
    return value


def construction_by_name(raw: object, context: str) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, list) or not raw:
        raise BenchmarkError(f"{context} construction stages must be a non-empty array")
    stages: dict[str, dict[str, Any]] = {}
    for stage in raw:
        if not isinstance(stage, dict):
            raise BenchmarkError(f"{context} construction stage must be an object")
        name = stage.get("name")
        if not isinstance(name, str) or not name or name in stages:
            raise BenchmarkError(
                f"{context} construction stage names must be unique non-empty strings"
            )
        stages[name] = stage
    return stages


def display_path(path: pathlib.Path) -> str:
    try:
        return str(path.resolve().relative_to(ROOT.resolve()))
    except ValueError:
        return str(path)


def build_report(
    manifest: dict[str, Any],
    observations: dict[str, Any],
    criterion_root: pathlib.Path | None,
    manifest_path: pathlib.Path = MANIFEST_PATH,
) -> dict[str, Any]:
    if manifest.get("schema_version") != 2 or observations.get("schema_version") != 2:
        raise BenchmarkError("unsupported vector-search benchmark schema")
    profile_name = observations.get("profile")
    profiles = profiles_by_name(manifest.get("profiles"))
    if manifest.get("default_profile") not in profiles:
        raise BenchmarkError("manifest default_profile must name a declared profile")
    if not isinstance(profile_name, str) or profile_name not in profiles:
        raise BenchmarkError("observations select an unknown manifest profile")
    profile = profiles[profile_name]
    storage = manifest.get("storage")
    execution = manifest.get("execution")
    if storage != REQUIRED_STORAGE:
        raise BenchmarkError("manifest must require persistent SQLite with phase reopens")
    if not isinstance(execution, dict) or execution.get("api") != REQUIRED_SQL_API:
        raise BenchmarkError("manifest must require the Engine::sql execution boundary")
    if observations.get("storage") != storage or not isinstance(storage, dict):
        raise BenchmarkError("observed storage identity differs from the manifest")
    if observations.get("execution") != execution:
        raise BenchmarkError("observed SQL execution identity differs from the manifest")
    workload = profile.get("workload")
    if observations.get("workload") != workload or not isinstance(workload, dict):
        raise BenchmarkError("quality observations do not match the manifest workload identity")
    query_count = positive_integer(
        workload.get("quality_query_count"), "manifest quality_query_count"
    )
    performance_query_count = positive_integer(
        workload.get("performance_query_count"), "manifest performance_query_count"
    )
    if performance_query_count > query_count:
        raise BenchmarkError("performance_query_count cannot exceed quality_query_count")
    top_k = positive_integer(workload.get("top_k"), "manifest top_k")

    manifest_by_name = algorithms_by_name(profile.get("algorithms"), "manifest profile")
    observed_by_name = algorithms_by_name(observations.get("algorithms"), "observed")
    if set(manifest_by_name) != set(observed_by_name):
        raise BenchmarkError("manifest and observed algorithm identities differ")
    for name, algorithm in manifest_by_name.items():
        if observed_by_name[name].get("parameters") != algorithm.get("parameters"):
            raise BenchmarkError(f"observed parameters differ for {name}")

    ground_truth_name = manifest.get("ground_truth")
    if ground_truth_name not in observed_by_name:
        raise BenchmarkError("manifest ground_truth is not an observed algorithm")
    parsed = {
        name: parse_ranked_results(observed_by_name[name], query_count, top_k)
        for name in observed_by_name
    }
    exact = parsed[ground_truth_name]
    quality: dict[str, Any] = {}
    all_checks: list[dict[str, Any]] = []
    for name, algorithm in manifest_by_name.items():
        metrics = compute_quality_metrics(exact, parsed[name], top_k)
        checks = check_quality_gates(algorithm, metrics)
        quality[name] = {"parameters": algorithm.get("parameters", {}), "metrics": metrics, "checks": checks}
        all_checks.extend({"algorithm": name, **check} for check in checks)

    expected_construction = construction_by_name(
        profile.get("construction_stages"), "manifest profile"
    )
    observed_construction = construction_by_name(
        observations.get("construction"), "observed"
    )
    if set(expected_construction) != set(observed_construction):
        raise BenchmarkError("manifest and observed construction stage identities differ")
    construction: dict[str, Any] = {}
    corpus_size = positive_integer(workload.get("corpus_size"), "manifest corpus_size")
    for name, expected in expected_construction.items():
        observed = observed_construction[name]
        if observed.get("rows") != corpus_size:
            raise BenchmarkError(f"observed row count differs for construction stage {name}")
        if observed.get("statement") != expected.get("statement"):
            raise BenchmarkError(f"observed SQL statement identity differs for {name}")
        elapsed = finite_number(
            observed.get("elapsed_nanoseconds"), f"{name} elapsed_nanoseconds"
        )
        if elapsed <= 0.0:
            raise BenchmarkError(f"{name} elapsed_nanoseconds must be positive")
        construction[name] = {
            "rows": corpus_size,
            "statement": observed["statement"],
            "elapsed_nanoseconds": elapsed,
            "rows_per_second": corpus_size * 1.0e9 / elapsed,
        }

    performance: dict[str, Any] = {}
    if criterion_root is not None:
        measurement = profile.get("measurement")
        if not isinstance(measurement, dict):
            raise BenchmarkError("manifest profile measurement must be an object")
        estimator = measurement.get("criterion_point_estimator")
        if estimator not in {"mean", "slope"}:
            raise BenchmarkError("Criterion point estimator must be mean or slope")
        for name, algorithm in manifest_by_name.items():
            estimate = criterion_estimate(
                criterion_root, algorithm["criterion_benchmark"], estimator
            )
            latency = estimate / performance_query_count
            performance[name] = {
                "criterion_point_estimator": estimator,
                "criterion_point_estimate_nanoseconds_per_batch": estimate,
                "queries_per_batch": performance_query_count,
                "nanoseconds_per_query": latency,
                "queries_per_second": 1.0e9 / latency,
            }

    passed = all(check["passed"] for check in all_checks)
    return {
        "schema_version": 2,
        "generated_at_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        "manifest": display_path(manifest_path),
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "git_commit": git_value("rev-parse", "HEAD"),
        "git_dirty": bool(git_value("status", "--short")),
        "environment": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor() or "unknown",
            "rustc": command_value("rustc", "--version"),
        },
        "profile": profile_name,
        "storage": storage,
        "execution": execution,
        "workload": workload,
        "quality": quality,
        "performance": performance,
        "construction": construction,
        "checks": all_checks,
        "passed": passed,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=MANIFEST_PATH)
    parser.add_argument("--observations", type=pathlib.Path, default=DEFAULT_OBSERVATIONS)
    parser.add_argument("--criterion-root", type=pathlib.Path, default=ROOT / "target" / "criterion")
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--quality-only", action="store_true")
    args = parser.parse_args()

    manifest = load_json(args.manifest)
    observations = load_json(args.observations)
    criterion_root = None if args.quality_only else args.criterion_root
    report = build_report(manifest, observations, criterion_root, args.manifest)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    top_k = report["workload"]["top_k"]
    for name, result in report["quality"].items():
        metrics = result["metrics"]
        timing = report["performance"].get(name)
        timing_text = ""
        if timing is not None:
            timing_text = (
                f" latency={timing['nanoseconds_per_query'] / 1.0e3:.2f}us"
                f" qps={timing['queries_per_second']:.1f}"
            )
        print(
            f"{name}: recall@{top_k}={metrics['recall_at_k']:.4f} "
            f"top1={metrics['top_1_accuracy']:.4f} mrr@{top_k}={metrics['mrr_at_k']:.4f} "
            f"exact_set={metrics['exact_set_rate']:.4f}{timing_text}"
        )
    print(f"vector-search report: {args.output}")
    if not report["passed"]:
        failed = [check for check in report["checks"] if not check["passed"]]
        raise BenchmarkError(f"vector-search quality gates failed: {failed}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
