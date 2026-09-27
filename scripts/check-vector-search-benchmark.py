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
import itertools
import json
import math
import pathlib
import platform
import subprocess
import struct
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
    if observations.get("mode") == "correctness":
        if criterion_root is not None:
            raise BenchmarkError("correctness observations require --quality-only, without Criterion")
        return build_correctness_report(manifest, observations, manifest_path)
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
        "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
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


def f32(number: float) -> float:
    return struct.unpack("<f", struct.pack("<f", number))[0]


def correctness_vectors(spec: dict[str, Any], root: pathlib.Path):
    """Reconstruct inputs independently of the Rust executable and its scores."""
    dimensions = positive_integer(spec.get("dimensions"), "fixture dimensions")
    generator = spec.get("generator")
    if generator == "literal-tensor-v1":
        corpus, queries = spec["corpus"], spec["queries"]
    elif generator == "lcg-uniform-signed-f32-v1":

        def vector(seed):
            state = (seed + 0x9E3779B97F4A7C15) & ((1 << 64) - 1)
            values = []
            for _ in range(dimensions):
                state = (state * 6364136223846793005 + 1) & ((1 << 64) - 1)
                values.append(f32(f32(f32(f32(state >> 32) / f32(0xFFFFFFFF)) * 2) - 1))
            return values

        corpus = [
            [vector(spec["corpus_seed_start"] + index)]
            for index in range(spec["corpus_size"])
        ]
        queries = [
            vector(spec["query_seed_start"] + index)
            for index in range(spec["query_count"])
        ]
    elif generator == "frozen-f32-v1":
        path = (root / spec["fixture_manifest"]).resolve()
        try:
            path.relative_to(root.resolve())
        except ValueError as error:
            raise BenchmarkError(
                "fixture manifest escapes the workload directory"
            ) from error
        raw = path.read_bytes()
        if hashlib.sha256(raw).hexdigest() != spec.get("fixture_manifest_sha256"):
            raise BenchmarkError("frozen fixture manifest hash differs")
        frozen = json.loads(raw)
        if frozen["dimensions"] != dimensions:
            raise BenchmarkError("frozen fixture dimensions differ")

        def decode(kind):
            artifact = frozen["artifacts"][kind]
            artifact_path = path.parent / artifact["path"]
            if pathlib.Path(artifact["path"]).name != artifact["path"]:
                raise BenchmarkError("fixture artifact must use a plain filename")
            data = artifact_path.read_bytes()
            if (
                len(data) != artifact["bytes"]
                or len(data) != artifact["rows"] * dimensions * 4
                or hashlib.sha256(data).hexdigest() != artifact["sha256"]
            ):
                raise BenchmarkError("frozen fixture bytes or hash differ")
            return [list(row) for row in struct.iter_unpack(f"<{dimensions}f", data)]

        corpus, queries = [[row] for row in decode("corpus")], decode("queries")
    else:
        raise BenchmarkError(f"unknown correctness generator: {generator}")
    if len(corpus) != spec["corpus_size"] or len(queries) != spec["query_count"]:
        raise BenchmarkError("fixture row or query count differs")
    for row in [
        *queries,
        *(vector for tensor in corpus if tensor is not None for vector in tensor),
    ]:
        if len(row) != dimensions:
            raise BenchmarkError("fixture vector dimensions differ")
        for component in row:
            finite_number(component, "fixture component")
    return corpus, queries


def independent_cosine(left, right) -> float:
    denominator = math.sqrt(
        math.fsum(x * x for x in left) * math.fsum(x * x for x in right)
    )
    return (
        min(1.0, max(-1.0, math.fsum(x * y for x, y in zip(left, right)) / denominator))
        if denominator
        else 0.0
    )


def independent_probabilities(hits, parameters):
    distances = [1.0 - score for _, score in hits]
    ordered = sorted(distances)
    head = min(len(ordered) - 1, max(1, math.ceil(len(ordered) / 4)))
    mean = math.fsum(ordered) / len(ordered)
    variance = math.fsum((value - mean) ** 2 for value in ordered) / len(ordered)
    match_mean = math.fsum(ordered[:head]) / head if head else mean
    random_mean = math.fsum(ordered[head:]) / (len(ordered) - head)

    def probability(distance, good, random, sigma, prior):
        odds = ((random - distance) ** 2 - (good - distance) ** 2) / (2 * sigma**2)
        odds += math.log(prior / (1 - prior))
        return (
            1 / (1 + math.exp(-odds))
            if odds >= 0
            else math.exp(odds) / (1 + math.exp(odds))
        )

    fixed = [
        probability(
            distance,
            parameters["mu_match"],
            parameters["mu_random"],
            parameters["sigma"],
            parameters["base_rate"],
        )
        for distance in distances
    ]
    if (
        len(ordered) < 2
        or math.sqrt(variance) <= sys.float_info.epsilon
        or random_mean - match_mean <= sys.float_info.epsilon
    ):
        pool = [0.5] * len(ordered)
    else:
        pool = [
            min(
                1 - 1e-6,
                max(
                    1e-6,
                    probability(
                        distance, match_mean, random_mean, math.sqrt(variance), 0.5
                    ),
                ),
            )
            for distance in distances
        ]
    return pool, fixed


def validate_oracle_hits(parsed, oracle, *, exact: bool) -> float:
    errors = []
    for query_id, hits in parsed.items():
        scores = oracle[query_id]
        if any(doc_id not in scores for doc_id, _ in hits):
            raise BenchmarkError("returned identity is absent from the input vectors")
        errors.extend(abs(score - scores[doc_id]) for doc_id, score in hits)
        if exact:
            selected = {doc_id for doc_id, _ in hits}
            cutoff_id, _ = hits[-1]
            cutoff = scores[cutoff_id]
            if any(
                doc_id not in selected
                and (
                    score > cutoff + SCORE_TOLERANCE
                    or (score == cutoff and doc_id < cutoff_id)
                )
                for doc_id, score in scores.items()
            ):
                raise BenchmarkError(
                    "exact SQL ground truth omits an independently closer or canonical tied row"
                )
    error = max(errors, default=0.0)
    if exact and error > SCORE_TOLERANCE:
        raise BenchmarkError(
            "exact SQL scores differ from independent vector/tensor cosine"
        )
    return error


def unique_integers(values, context, minimum=1):
    if (
        not isinstance(values, list)
        or not values
        or any(type(value) is not int or value < minimum for value in values)
        or len(set(values)) != len(values)
    ):
        raise BenchmarkError(f"{context} must contain unique integers >= {minimum}")
    return values


def sensitivity(cases):
    result = {}
    for axis, stable in (
        ("candidate_k", "search_list_size"),
        ("search_list_size", "candidate_k"),
    ):
        differences = []
        for left, right in itertools.combinations(cases, 2):
            if (
                left["seed"] != right["seed"]
                or left[stable] != right[stable]
                or left[axis] == right[axis]
            ):
                continue
            lhits = {
                (row["query_id"], hit["doc_id"]): hit
                for row in left["results"]
                for hit in row["hits"]
            }
            rhits = {
                (row["query_id"], hit["doc_id"]): hit
                for row in right["results"]
                for hit in row["hits"]
            }
            differences.extend(
                (
                    abs(
                        lhits[key]["pool_probability"] - rhits[key]["pool_probability"]
                    ),
                    abs(
                        lhits[key]["fixed_probability"]
                        - rhits[key]["fixed_probability"]
                    ),
                )
                for key in lhits.keys() & rhits.keys()
            )
        if not differences:
            raise BenchmarkError(f"no shared observations for {axis} sensitivity")
        result[axis] = {
            "shared_comparisons": len(differences),
            "mean_pool_probability_shift": math.fsum(pair[0] for pair in differences)
            / len(differences),
            "max_pool_probability_shift": max(pair[0] for pair in differences),
            "max_fixed_probability_shift": max(pair[1] for pair in differences),
        }
    return result


def correctness_fixture(spec, observed, suite, root):
    corpus, queries = correctness_vectors(spec, root)
    oracle = {
        query_id: {
            doc_id: max(independent_cosine(vector, query) for vector in tensor)
            for doc_id, tensor in enumerate(corpus, 1)
            if tensor
        }
        for query_id, query in enumerate(queries)
    }
    ks = unique_integers(spec.get("candidate_ks"), "candidate ks", 2)
    exact = {}
    for raw in observed.get("exact", []):
        k = raw.get("candidate_k")
        if type(k) is not int or k not in ks or k in exact:
            raise BenchmarkError("exact candidate-k identities differ")
        exact[k] = parse_ranked_results(
            {"name": "exact", "results": raw.get("results")}, len(queries), k
        )
        validate_oracle_hits(exact[k], oracle, exact=True)
    if set(exact) != set(ks):
        raise BenchmarkError("missing exact candidate-k observations")
    expected = set(itertools.product(suite["seeds"], suite["search_list_sizes"], ks))
    seen, quality, checks = set(), [], []
    cases = observed.get("cases", [])
    for case in cases:
        identity = (
            case.get("seed"),
            case.get("search_list_size"),
            case.get("candidate_k"),
        )
        if (
            any(type(value) is not int for value in identity)
            or identity not in expected
            or identity in seen
        ):
            raise BenchmarkError("unexpected or duplicate seed/search/k case")
        seen.add(identity)
        seed, search_list, k = identity
        diagnostic = case.get("diagnostic", {})
        if (
            diagnostic.get("route") != "approximate"
            or diagnostic.get("requested_k") != k
            or diagnostic.get("returned_documents") != k
            or diagnostic.get("exact_vectors") != 0
            or positive_integer(diagnostic.get("pq_estimates"), "PQ estimates") == 0
            or diagnostic.get("generation") is None
        ):
            raise BenchmarkError("missing actual approximate-route diagnostic")
        parsed = parse_ranked_results(
            {"name": "diskann", "results": case.get("results")}, len(queries), k
        )
        metrics = compute_quality_metrics(exact[k], parsed, k)
        metrics["max_oracle_score_abs_error"] = validate_oracle_hits(
            parsed, oracle, exact=False
        )
        errors = []
        for row in case["results"]:
            pool, fixed = independent_probabilities(
                parsed[row["query_id"]], suite["fixed_transform"]
            )
            for hit, expected_pool, expected_fixed in zip(row["hits"], pool, fixed):
                for field, target in (
                    ("pool_probability", expected_pool),
                    ("fixed_probability", expected_fixed),
                ):
                    actual = finite_number(hit.get(field), field)
                    if not 0 <= actual <= 1:
                        raise BenchmarkError("probability must be in [0, 1]")
                    errors.append(abs(actual - target))
        metrics["max_probability_abs_error"] = max(errors)
        # Equality here is literal: approximate tie credit does not replace canonical ID recall.
        metrics["tie_aware_recall_at_k"] = sum(
            sum(
                oracle[query_id][doc_id] >= oracle[query_id][exact[k][query_id][-1][0]]
                for doc_id, _ in hits
            )
            for query_id, hits in parsed.items()
        ) / (len(queries) * k)
        current = check_quality_gates(spec, metrics)
        checks.extend(
            {
                "fixture": spec["name"],
                "seed": seed,
                "search_list_size": search_list,
                "candidate_k": k,
                **check,
            }
            for check in current
        )
        quality.append(
            {
                "seed": seed,
                "search_list_size": search_list,
                "candidate_k": k,
                "metrics": metrics,
            }
        )
    if seen != expected:
        raise BenchmarkError("missing declared seed/search/k cases")
    shifts = sensitivity(cases)
    for axis, values in shifts.items():
        actual = values["max_fixed_probability_shift"]
        limit = spec["maximum_quality"]["max_probability_abs_error"]
        checks.append(
            {
                "fixture": spec["name"],
                "axis": axis,
                "metric": "max_fixed_probability_shift",
                "relation": "<=",
                "actual": actual,
                "limit": limit,
                "passed": actual <= limit,
            }
        )
    variation = []
    for search_list, k in itertools.product(suite["search_list_sizes"], ks):
        sample = [
            item["metrics"]
            for item in quality
            if item["search_list_size"] == search_list and item["candidate_k"] == k
        ]
        variation.append(
            {
                "search_list_size": search_list,
                "candidate_k": k,
                "seeds": suite["seeds"],
                **{
                    metric: {
                        "minimum": min(item[metric] for item in sample),
                        "maximum": max(item[metric] for item in sample),
                    }
                    for metric in ("recall_at_k", "top_1_accuracy")
                },
            }
        )
    return {
        "name": spec["name"],
        "quality": quality,
        "seed_variation": variation,
        "probability_sensitivity": shifts,
    }, checks


def build_correctness_report(manifest, observations, manifest_path):
    if manifest.get("schema_version") != 2 or observations.get("schema_version") != 3:
        raise BenchmarkError("unsupported correctness schema")
    suite = manifest.get("correctness")
    if (
        not isinstance(suite, dict)
        or suite.get("schema_version") != 1
        or observations.get("suite") != suite
    ):
        raise BenchmarkError("correctness contract differs from the manifest")
    if (
        suite.get("storage") != REQUIRED_STORAGE
        or suite.get("execution", {}).get("api") != REQUIRED_SQL_API
    ):
        raise BenchmarkError(
            "correctness requires persistent reopened SQLite and Engine::sql"
        )
    if (
        suite.get("execution", {}).get("fixed_transform_api")
        != "VectorProbabilityTransform::calibrate_one"
        or suite.get("empirical_calibration") is not False
    ):
        raise BenchmarkError(
            "fixed-transform checks cannot claim empirical calibration"
        )

    def reject_timing(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key.startswith(
                    ("elapsed", "timing", "performance", "criterion", "construction")
                ):
                    raise BenchmarkError(
                        "correctness observations must not contain timing measurements"
                    )
                reject_timing(child)
        elif isinstance(value, list):
            for child in value:
                reject_timing(child)

    reject_timing(observations)
    manifest_hash = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    if observations.get("manifest_sha256") != manifest_hash:
        raise BenchmarkError("compiled workload manifest hash differs")
    artifact = observations.get("executable_sha256")
    if (
        not isinstance(artifact, str)
        or len(artifact) != 64
        or any(char not in "0123456789abcdef" for char in artifact)
    ):
        raise BenchmarkError("executable SHA-256 is required")
    unique_integers(suite.get("seeds"), "seeds", 0)
    unique_integers(suite.get("search_list_sizes"), "search list sizes")
    specs = algorithms_by_name(suite.get("fixtures"), "correctness fixture")
    observed = algorithms_by_name(observations.get("fixtures"), "observed fixture")
    if set(specs) != set(observed):
        raise BenchmarkError("correctness fixture identities differ")
    results, checks = [], []
    for name, spec in specs.items():
        required_minimum = {"recall_at_k", "top_1_accuracy", "result_count_rate"}
        required_maximum = {
            "max_shared_score_abs_error",
            "max_oracle_score_abs_error",
            "max_probability_abs_error",
        }
        if (
            not required_minimum <= spec.get("minimum_quality", {}).keys()
            or not required_maximum <= spec.get("maximum_quality", {}).keys()
        ):
            raise BenchmarkError("correctness fixture omits a required quality gate")
        result, current = correctness_fixture(
            spec, observed[name], suite, manifest_path.parent
        )
        results.append(result)
        checks.extend(current)
    return {
        "schema_version": 3,
        "mode": "correctness",
        "manifest_sha256": manifest_hash,
        "executable_sha256": artifact,
        "git_commit": git_value("rev-parse", "HEAD"),
        "git_dirty": bool(git_value("status", "--short")),
        "empirical_calibration": False,
        "fixtures": results,
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
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

    if report.get("mode") == "correctness":
        for fixture in report["fixtures"]:
            print(f"{fixture['name']}: {len(fixture['quality'])} correctness cases")
    top_k = report.get("workload", {}).get("top_k")
    for name, result in report.get("quality", {}).items():
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
