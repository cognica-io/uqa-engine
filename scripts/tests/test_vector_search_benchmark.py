#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
CHECKER = ROOT / "scripts" / "check-vector-search-benchmark.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("vector_search_benchmark", CHECKER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class VectorSearchBenchmarkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.checker = load_checker()

    def test_quality_metrics_compare_ranked_results_with_exact_ground_truth(self) -> None:
        exact = {
            0: [(1, 0.9), (2, 0.8)],
            1: [(4, 0.95), (5, 0.8)],
        }
        candidate = {
            0: [(1, 0.9), (3, 0.7)],
            1: [(5, 0.8), (6, 0.7)],
        }
        metrics = self.checker.compute_quality_metrics(exact, candidate, 2)
        self.assertEqual(metrics["recall_at_k"], 0.5)
        self.assertEqual(metrics["top_1_accuracy"], 0.5)
        self.assertEqual(metrics["mrr_at_k"], 0.5)
        self.assertEqual(metrics["exact_set_rate"], 0.0)
        self.assertEqual(metrics["result_count_rate"], 1.0)
        self.assertAlmostEqual(metrics["mean_top_1_similarity_loss"], 0.075)
        self.assertEqual(metrics["max_shared_score_abs_error"], 0.0)

    def test_shared_score_error_is_measured_for_recalled_documents(self) -> None:
        exact = {0: [(1, 0.9), (2, 0.8)]}
        candidate = {0: [(1, 0.89), (3, 0.7)]}
        metrics = self.checker.compute_quality_metrics(exact, candidate, 2)
        self.assertAlmostEqual(metrics["mean_top_1_similarity_loss"], 0.01)
        self.assertAlmostEqual(metrics["max_shared_score_abs_error"], 0.01)

    def test_parser_rejects_non_ranked_and_duplicate_hits(self) -> None:
        non_ranked = {
            "name": "candidate",
            "results": [
                {
                    "query_id": 0,
                    "hits": [
                        {"doc_id": 1, "score": 0.7},
                        {"doc_id": 2, "score": 0.8},
                    ],
                }
            ],
        }
        with self.assertRaisesRegex(self.checker.BenchmarkError, "rank order"):
            self.checker.parse_ranked_results(non_ranked, 1, 2)

        duplicate = {
            "name": "candidate",
            "results": [
                {
                    "query_id": 0,
                    "hits": [
                        {"doc_id": 1, "score": 0.8},
                        {"doc_id": 1, "score": 0.7},
                    ],
                }
            ],
        }
        with self.assertRaisesRegex(self.checker.BenchmarkError, "repeats doc_id"):
            self.checker.parse_ranked_results(duplicate, 1, 2)

    def test_quality_gates_report_each_failed_bound(self) -> None:
        algorithm = {
            "name": "candidate",
            "minimum_quality": {"recall_at_k": 0.9},
            "maximum_quality": {"mean_top_1_similarity_loss": 0.01},
        }
        metrics = {"recall_at_k": 0.8, "mean_top_1_similarity_loss": 0.02}
        checks = self.checker.check_quality_gates(algorithm, metrics)
        self.assertEqual([check["passed"] for check in checks], [False, False])

    def test_schema_two_report_selects_profile_and_validates_persistent_sql(self) -> None:
        manifest, observations = self.payloads()
        report = self.checker.build_report(
            manifest, observations, None, ROOT / "benchmarks/vector-search/manifest.json"
        )
        self.assertEqual(report["profile"], "test")
        self.assertEqual(report["storage"]["backend"], "sqlite")
        self.assertEqual(report["execution"]["api"], "Engine::sql")
        self.assertEqual(report["construction"]["sql_load"]["rows_per_second"], 200.0)

        observations["storage"] = {"backend": "memory"}
        with self.assertRaisesRegex(self.checker.BenchmarkError, "storage identity"):
            self.checker.build_report(
                manifest,
                observations,
                None,
                ROOT / "benchmarks/vector-search/manifest.json",
            )

        manifest["storage"] = observations["storage"]
        with self.assertRaisesRegex(self.checker.BenchmarkError, "persistent SQLite"):
            self.checker.build_report(
                manifest,
                observations,
                None,
                ROOT / "benchmarks/vector-search/manifest.json",
            )

    def test_per_query_latency_uses_performance_batch_size(self) -> None:
        manifest, observations = self.payloads()
        with tempfile.TemporaryDirectory() as directory:
            estimates = (
                pathlib.Path(directory)
                / "sql_vector_search_query_batch/test/exact/new/estimates.json"
            )
            estimates.parent.mkdir(parents=True)
            estimates.write_text(
                json.dumps({"mean": {"point_estimate": 2_000.0}}), encoding="utf-8"
            )
            report = self.checker.build_report(
                manifest,
                observations,
                pathlib.Path(directory),
                ROOT / "benchmarks/vector-search/manifest.json",
            )
        self.assertEqual(report["performance"]["exact"]["queries_per_batch"], 2)
        self.assertEqual(report["performance"]["exact"]["nanoseconds_per_query"], 1_000.0)

    @staticmethod
    def payloads():
        storage = {
            "backend": "sqlite",
            "persistent": True,
            "reopened_before_each_query_phase": True,
        }
        execution = {
            "api": "Engine::sql",
            "query": "sql",
            "index_lifecycle": "SQL CREATE INDEX / DROP INDEX",
        }
        workload = {
            "corpus_size": 2,
            "dimensions": 2,
            "quality_query_count": 2,
            "performance_query_count": 2,
            "top_k": 1,
        }
        algorithm = {
            "name": "exact",
            "parameters": {"access_method": "sqlite-bruteforce"},
            "criterion_benchmark": "sql_vector_search_query_batch/test/exact",
            "minimum_quality": {"recall_at_k": 1.0},
        }
        manifest = {
            "schema_version": 2,
            "default_profile": "test",
            "storage": storage,
            "execution": execution,
            "ground_truth": "exact",
            "profiles": [
                {
                    "name": "test",
                    "workload": workload,
                    "algorithms": [algorithm],
                    "construction_stages": [
                        {"name": "sql_load", "statement": "SQL INSERT"}
                    ],
                    "measurement": {"criterion_point_estimator": "mean"},
                }
            ],
        }
        observations = {
            "schema_version": 2,
            "profile": "test",
            "storage": storage,
            "execution": execution,
            "workload": workload,
            "algorithms": [
                {
                    "name": "exact",
                    "parameters": {"access_method": "sqlite-bruteforce"},
                    "results": [
                        {"query_id": 0, "hits": [{"doc_id": 1, "score": 0.9}]},
                        {"query_id": 1, "hits": [{"doc_id": 0, "score": 0.8}]},
                    ],
                }
            ],
            "construction": [
                {
                    "name": "sql_load",
                    "rows": 2,
                    "statement": "SQL INSERT",
                    "elapsed_nanoseconds": 10_000_000,
                }
            ],
        }
        return manifest, observations


if __name__ == "__main__":
    unittest.main()
