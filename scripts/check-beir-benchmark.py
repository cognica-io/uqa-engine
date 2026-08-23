#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate BEIR SQL hybrid-search quality and Criterion measurements."""

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
DEFAULT_MANIFEST = ROOT / "benchmarks" / "beir" / "manifest.json"
DEFAULT_OBSERVATIONS = ROOT / "target" / "benchmark-runs" / "beir-observations.json"
DEFAULT_OUTPUT = ROOT / "target" / "benchmark-runs" / "beir-report.json"
SCORE_TOLERANCE = 1.0e-9


class BenchmarkError(RuntimeError):
    """A malformed or failed BEIR benchmark report."""


def load_json(path: pathlib.Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise BenchmarkError(f"JSON root must be an object: {path}")
    return value


def finite_number(value: object, context: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BenchmarkError(f"{context} must be numeric")
    number = float(value)
    if not math.isfinite(number):
        raise BenchmarkError(f"{context} must be finite")
    return number


def positive_integer(value: object, context: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise BenchmarkError(f"{context} must be a positive integer")
    return value


def objects_by_name(raw: object, context: str) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, list) or not raw:
        raise BenchmarkError(f"{context} must be a non-empty array")
    values: dict[str, dict[str, Any]] = {}
    for value in raw:
        if not isinstance(value, dict):
            raise BenchmarkError(f"{context} entry must be an object")
        name = value.get("name")
        if not isinstance(name, str) or not name or name in values:
            raise BenchmarkError(f"{context} names must be unique non-empty strings")
        values[name] = value
    return values


def parse_judgments(raw: object, query_count: int, corpus_count: int) -> dict[str, dict[int, float]]:
    if not isinstance(raw, list) or len(raw) != query_count:
        raise BenchmarkError("observed query judgments do not match the declared query count")
    queries: dict[str, dict[int, float]] = {}
    for query in raw:
        if not isinstance(query, dict):
            raise BenchmarkError("query judgment entry must be an object")
        query_id = query.get("query_id")
        judgments = query.get("judgments")
        if not isinstance(query_id, str) or not query_id or query_id in queries:
            raise BenchmarkError("query IDs must be unique non-empty strings")
        if not isinstance(judgments, dict) or not judgments:
            raise BenchmarkError(f"query {query_id} must have judgments")
        parsed: dict[int, float] = {}
        for raw_document_id, raw_score in judgments.items():
            try:
                document_id = int(raw_document_id)
            except (TypeError, ValueError) as error:
                raise BenchmarkError(f"query {query_id} has invalid document ID") from error
            score = finite_number(raw_score, f"query {query_id} judgment")
            if not 1 <= document_id <= corpus_count or score <= 0.0:
                raise BenchmarkError(f"query {query_id} has invalid judgment")
            parsed[document_id] = score
        queries[query_id] = parsed
    return queries


def validate_score(score: float, domain: str, context: str) -> None:
    if domain == "cosine" and not -1.0 - SCORE_TOLERANCE <= score <= 1.0 + SCORE_TOLERANCE:
        raise BenchmarkError(f"{context} is outside the cosine domain")
    if domain == "probability" and not 0.0 - SCORE_TOLERANCE <= score <= 1.0 + SCORE_TOLERANCE:
        raise BenchmarkError(f"{context} is outside the probability domain")
    if domain == "nonnegative" and score < -SCORE_TOLERANCE:
        raise BenchmarkError(f"{context} is negative")
    if domain not in {"cosine", "probability", "nonnegative"}:
        raise BenchmarkError(f"unknown score domain {domain!r}")


def parse_system_results(
    observed: dict[str, Any],
    specification: dict[str, Any],
    query_ids: set[str],
    top_k: int,
) -> dict[str, list[int]]:
    name = specification["name"]
    domain = specification.get("score_domain")
    raw_results = observed.get("results")
    if not isinstance(raw_results, list) or len(raw_results) != len(query_ids):
        raise BenchmarkError(f"{name} result count differs from the query set")
    results: dict[str, list[int]] = {}
    for result in raw_results:
        if not isinstance(result, dict):
            raise BenchmarkError(f"{name} result must be an object")
        query_id = result.get("query_id")
        hits = result.get("hits")
        if not isinstance(query_id, str) or query_id in results or query_id not in query_ids:
            raise BenchmarkError(f"{name} has an invalid or repeated query ID")
        if not isinstance(hits, list) or len(hits) > top_k:
            raise BenchmarkError(f"{name} query {query_id} exceeds top-k")
        if specification.get("require_top_k") is True and len(hits) != top_k:
            raise BenchmarkError(f"{name} query {query_id} must contain exactly top-k hits")
        ranked: list[tuple[int, float]] = []
        seen: set[int] = set()
        for rank, hit in enumerate(hits, start=1):
            if not isinstance(hit, dict):
                raise BenchmarkError(f"{name} query {query_id} rank {rank} is not an object")
            document_id = hit.get("doc_id")
            if isinstance(document_id, bool) or not isinstance(document_id, int) or document_id <= 0:
                raise BenchmarkError(f"{name} query {query_id} rank {rank} has invalid doc_id")
            if document_id in seen:
                raise BenchmarkError(f"{name} query {query_id} repeats doc_id {document_id}")
            score = finite_number(hit.get("score"), f"{name} query {query_id} rank {rank}")
            validate_score(score, domain, f"{name} query {query_id} rank {rank}")
            seen.add(document_id)
            ranked.append((document_id, score))
        if ranked != sorted(ranked, key=lambda item: (-item[1], item[0])):
            raise BenchmarkError(f"{name} query {query_id} has non-deterministic rank order")
        results[query_id] = [document_id for document_id, _ in ranked]
    if set(results) != query_ids:
        raise BenchmarkError(f"{name} query IDs differ from the judgment set")
    return results


def dcg(relevances: list[float], k: int) -> float:
    return sum(gain / math.log2(rank + 2) for rank, gain in enumerate(relevances[:k]))


def quality_metrics(
    judgments: dict[str, dict[int, float]],
    ranked: dict[str, list[int]],
    k: int,
) -> dict[str, float]:
    if set(judgments) != set(ranked) or not judgments:
        raise BenchmarkError("ranked results and judgments must have identical non-empty queries")
    ndcg_total = 0.0
    average_precision_total = 0.0
    recall_total = 0.0
    reciprocal_rank_total = 0.0
    for query_id, query_judgments in judgments.items():
        relevances = [query_judgments.get(document_id, 0.0) for document_id in ranked[query_id][:k]]
        ideal = sorted(query_judgments.values(), reverse=True)
        ideal_dcg = dcg(ideal, k)
        ndcg_total += dcg(relevances, k) / ideal_dcg if ideal_dcg else 0.0
        relevant_count = sum(score > 0.0 for score in query_judgments.values())
        hits = 0
        precision_sum = 0.0
        first_relevant_rank = 0
        for rank, relevance in enumerate(relevances, start=1):
            if relevance > 0.0:
                hits += 1
                precision_sum += hits / rank
                if first_relevant_rank == 0:
                    first_relevant_rank = rank
        average_precision_total += precision_sum / min(relevant_count, k)
        recall_total += hits / relevant_count
        if first_relevant_rank:
            reciprocal_rank_total += 1.0 / first_relevant_rank
    count = len(judgments)
    return {
        f"ndcg_at_{k}": ndcg_total / count,
        f"map_at_{k}": average_precision_total / count,
        f"recall_at_{k}": recall_total / count,
        f"mrr_at_{k}": reciprocal_rank_total / count,
    }


def criterion_estimate(criterion_root: pathlib.Path, benchmark: str, estimator: str) -> float:
    path = criterion_root.joinpath(*benchmark.split("/"), "new", "estimates.json")
    payload = load_json(path)
    try:
        value = payload[estimator]["point_estimate"]
    except (KeyError, TypeError) as error:
        raise BenchmarkError(f"missing Criterion {estimator} estimate in {path}") from error
    estimate = finite_number(value, f"Criterion estimate for {benchmark}")
    if estimate <= 0.0:
        raise BenchmarkError(f"Criterion estimate for {benchmark} must be positive")
    return estimate


def validate_identity(manifest: dict[str, Any], observations: dict[str, Any], manifest_path: pathlib.Path) -> None:
    if manifest.get("schema_version") != 1 or observations.get("schema_version") != 1:
        raise BenchmarkError("unsupported BEIR benchmark schema")
    expected_hash = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    if observations.get("benchmark_manifest_sha256") != expected_hash:
        raise BenchmarkError("prepared data or observations use a different benchmark manifest")
    for key in ("dataset", "embedding", "storage", "execution", "indexes", "workload"):
        if observations.get(key) != manifest.get(key):
            raise BenchmarkError(f"observed {key} identity differs from the manifest")
    storage = manifest.get("storage")
    execution = manifest.get("execution")
    if not isinstance(storage, dict) or storage.get("backend") != "sqlite" or storage.get("persistent") is not True:
        raise BenchmarkError("BEIR benchmark must use persistent SQLite")
    if not isinstance(execution, dict) or execution.get("api") != "Engine::sql":
        raise BenchmarkError("BEIR benchmark must execute through Engine::sql")
    if storage.get("reopened_before_query_phase") is not True:
        raise BenchmarkError("BEIR database must reopen before the query phase")


def construction_report(manifest: dict[str, Any], observations: dict[str, Any]) -> dict[str, Any]:
    execution = manifest["execution"]
    indexes = objects_by_name(manifest.get("indexes"), "manifest indexes")
    expected = {"sql_load": execution["load"]}
    expected.update({name: index["statement"] for name, index in indexes.items()})
    observed = objects_by_name(observations.get("construction"), "observed construction")
    if set(observed) != set(expected):
        raise BenchmarkError("construction stage identities differ from the manifest")
    corpus_count = positive_integer(manifest["dataset"].get("expected_corpus_count"), "corpus count")
    report: dict[str, Any] = {}
    for name, statement in expected.items():
        stage = observed[name]
        if stage.get("statement") != statement or stage.get("rows") != corpus_count:
            raise BenchmarkError(f"construction stage {name} differs from the manifest")
        elapsed = finite_number(stage.get("elapsed_nanoseconds"), f"{name} elapsed time")
        if elapsed <= 0.0:
            raise BenchmarkError(f"{name} elapsed time must be positive")
        report[name] = {
            "statement": statement,
            "rows": corpus_count,
            "elapsed_nanoseconds": elapsed,
            "rows_per_second": corpus_count * 1.0e9 / elapsed,
        }
    return report


def validate_preparation(manifest: dict[str, Any], observations: dict[str, Any]) -> None:
    preparation = observations.get("preparation")
    artifacts = observations.get("artifacts")
    if not isinstance(preparation, dict) or not isinstance(artifacts, dict):
        raise BenchmarkError("BEIR preparation metadata and artifacts are required")
    if not isinstance(preparation.get("archive_cache_hit"), bool):
        raise BenchmarkError("preparation archive_cache_hit must be boolean")
    for name in ("download_seconds", "corpus_embedding_seconds", "query_embedding_seconds"):
        elapsed = finite_number(preparation.get(name), f"preparation {name}")
        if elapsed < 0.0 or (name != "download_seconds" and elapsed <= 0.0):
            raise BenchmarkError(f"preparation {name} has an invalid duration")
    expected_rows = {
        "corpus": positive_integer(
            manifest["dataset"].get("expected_corpus_count"), "corpus count"
        ),
        "queries": positive_integer(
            manifest["dataset"].get("expected_query_count"), "query count"
        ),
    }
    if set(artifacts) != set(expected_rows):
        raise BenchmarkError("prepared artifact identities must be corpus and queries")
    for name, rows in expected_rows.items():
        artifact = artifacts[name]
        if not isinstance(artifact, dict) or artifact.get("rows") != rows:
            raise BenchmarkError(f"prepared {name} artifact row count differs")
        path = artifact.get("path")
        digest = artifact.get("sha256")
        if not isinstance(path, str) or pathlib.PurePath(path).name != path:
            raise BenchmarkError(f"prepared {name} artifact path must be a file name")
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise BenchmarkError(f"prepared {name} artifact SHA-256 is invalid")


def build_report(
    manifest: dict[str, Any],
    observations: dict[str, Any],
    criterion_root: pathlib.Path | None,
    manifest_path: pathlib.Path = DEFAULT_MANIFEST,
) -> dict[str, Any]:
    validate_identity(manifest, observations, manifest_path)
    validate_preparation(manifest, observations)
    workload = manifest["workload"]
    query_count = positive_integer(workload.get("quality_query_count"), "quality query count")
    performance_count = positive_integer(
        workload.get("performance_query_count"), "performance query count"
    )
    top_k = positive_integer(workload.get("top_k"), "top_k")
    if performance_count > query_count:
        raise BenchmarkError("performance query count exceeds quality query count")
    corpus_count = positive_integer(manifest["dataset"].get("expected_corpus_count"), "corpus count")
    judgments = parse_judgments(observations.get("queries"), query_count, corpus_count)
    specifications = objects_by_name(manifest.get("systems"), "manifest systems")
    observed_systems = objects_by_name(observations.get("systems"), "observed systems")
    if set(specifications) != set(observed_systems):
        raise BenchmarkError("observed systems differ from the manifest")

    quality: dict[str, Any] = {}
    checks: list[dict[str, Any]] = []
    for name, specification in specifications.items():
        ranked = parse_system_results(observed_systems[name], specification, set(judgments), top_k)
        metrics = quality_metrics(judgments, ranked, top_k)
        system_checks = []
        minimums = specification.get("minimum_quality")
        if not isinstance(minimums, dict) or not minimums:
            raise BenchmarkError(f"{name} must declare quality floors")
        for metric, raw_minimum in minimums.items():
            if metric not in metrics:
                raise BenchmarkError(f"{name} declares unknown quality metric {metric}")
            minimum = finite_number(raw_minimum, f"{name} {metric} floor")
            actual = metrics[metric]
            check = {
                "name": f"{name}_{metric}",
                "system": name,
                "metric": metric,
                "relation": ">=",
                "limit": minimum,
                "actual": actual,
                "passed": actual + 1.0e-12 >= minimum,
            }
            system_checks.append(check)
            checks.append(check)
        quality[name] = {"metrics": metrics, "checks": system_checks}

    comparisons = manifest.get("comparative_quality")
    if not isinstance(comparisons, list):
        raise BenchmarkError("comparative_quality must be an array")
    for comparison in comparisons:
        if not isinstance(comparison, dict):
            raise BenchmarkError("comparative quality entry must be an object")
        system = comparison.get("system")
        metric = comparison.get("metric")
        baselines = comparison.get("baseline_systems")
        if system not in quality or not isinstance(metric, str) or not isinstance(baselines, list):
            raise BenchmarkError("invalid comparative quality identity")
        if not baselines or any(baseline not in quality for baseline in baselines):
            raise BenchmarkError("invalid comparative quality baseline")
        minimum_delta = finite_number(comparison.get("minimum_delta"), "comparative minimum_delta")
        actual = quality[system]["metrics"].get(metric)
        baseline = max(quality[name]["metrics"].get(metric, -math.inf) for name in baselines)
        if actual is None or not math.isfinite(baseline):
            raise BenchmarkError("comparative quality metric is unavailable")
        check = {
            "name": comparison.get("name"),
            "system": system,
            "metric": metric,
            "relation": ">= best baseline + delta",
            "limit": baseline + minimum_delta,
            "actual": actual,
            "passed": actual + 1.0e-12 >= baseline + minimum_delta,
        }
        checks.append(check)

    performance: dict[str, Any] = {}
    if criterion_root is not None:
        measurement = manifest.get("measurement")
        if not isinstance(measurement, dict) or measurement.get("criterion_point_estimator") != "mean":
            raise BenchmarkError("BEIR measurement must use the Criterion mean")
        for name, specification in specifications.items():
            estimate = criterion_estimate(
                criterion_root, specification["criterion_benchmark"], "mean"
            )
            latency = estimate / performance_count
            performance[name] = {
                "criterion_point_estimate_nanoseconds_per_batch": estimate,
                "queries_per_batch": performance_count,
                "nanoseconds_per_query": latency,
                "queries_per_second": 1.0e9 / latency,
            }

    return {
        "schema_version": 1,
        "generated_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "manifest": display_path(manifest_path),
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "git_commit": command_value("git", "rev-parse", "HEAD"),
        "git_dirty": bool(command_value("git", "status", "--short")),
        "environment": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "processor": platform.processor() or "unknown",
            "rustc": command_value("rustc", "--version"),
        },
        "dataset": manifest["dataset"],
        "embedding": manifest["embedding"],
        "storage": manifest["storage"],
        "execution": manifest["execution"],
        "workload": workload,
        "preparation": observations.get("preparation"),
        "artifacts": observations.get("artifacts"),
        "quality": quality,
        "performance": performance,
        "construction": construction_report(manifest, observations),
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
    }


def command_value(*command: str) -> str:
    process = subprocess.run(command, cwd=ROOT, check=False, capture_output=True, text=True)
    return process.stdout.strip() if process.returncode == 0 else "unknown"


def display_path(path: pathlib.Path) -> str:
    try:
        return str(path.resolve().relative_to(ROOT.resolve()))
    except ValueError:
        return str(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--observations", type=pathlib.Path, default=DEFAULT_OBSERVATIONS)
    parser.add_argument("--criterion-root", type=pathlib.Path, default=ROOT / "target" / "criterion")
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--quality-only", action="store_true")
    args = parser.parse_args()
    manifest = load_json(args.manifest)
    observations = load_json(args.observations)
    report = build_report(
        manifest,
        observations,
        None if args.quality_only else args.criterion_root,
        args.manifest,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    top_k = report["workload"]["top_k"]
    for name, result in report["quality"].items():
        metrics = result["metrics"]
        timing = report["performance"].get(name)
        timing_text = ""
        if timing:
            timing_text = (
                f" latency={timing['nanoseconds_per_query'] / 1.0e6:.2f}ms"
                f" qps={timing['queries_per_second']:.1f}"
            )
        print(
            f"{name}: NDCG@{top_k}={metrics[f'ndcg_at_{top_k}']:.4f} "
            f"MAP@{top_k}={metrics[f'map_at_{top_k}']:.4f} "
            f"Recall@{top_k}={metrics[f'recall_at_{top_k}']:.4f}{timing_text}"
        )
    print(f"BEIR report: {args.output}")
    if not report["passed"]:
        failed = [check for check in report["checks"] if not check["passed"]]
        raise BenchmarkError(f"BEIR quality gates failed: {failed}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
