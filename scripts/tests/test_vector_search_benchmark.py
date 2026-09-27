#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import copy
import hashlib
import itertools
import json
import pathlib
import struct
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


class DiskANNCorrectnessTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.checker = load_checker()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.manifest_path = pathlib.Path(self.directory.name) / "manifest.json"
        fixture = {
            "name": "literal",
            "generator": "literal-tensor-v1",
            "dimensions": 2,
            "corpus_size": 4,
            "query_count": 1,
            "candidate_ks": [2, 3],
            "corpus": [[[0, 1], [1, 0]], [[0, 1]], [[-1, 0]], None],
            "queries": [[1, 0]],
            "minimum_quality": {
                "recall_at_k": 1.0,
                "top_1_accuracy": 1.0,
                "result_count_rate": 1.0,
            },
            "maximum_quality": {
                "max_shared_score_abs_error": 1e-6,
                "max_oracle_score_abs_error": 1e-6,
                "max_probability_abs_error": 1e-10,
            },
        }
        suite = {
            "schema_version": 1,
            "name": "literal-correctness",
            "storage": self.checker.REQUIRED_STORAGE,
            "execution": {
                "api": "Engine::sql",
                "fixed_transform_api": "VectorProbabilityTransform::calibrate_one",
            },
            "seeds": [7, 42],
            "search_list_sizes": [2, 4],
            "fixed_transform": {
                "mu_match": 0.0,
                "mu_random": 1.0,
                "sigma": 1.0,
                "base_rate": 0.5,
            },
            "empirical_calibration": False,
            "fixtures": [fixture],
        }
        self.manifest = {"schema_version": 2, "correctness": suite}
        self.manifest_path.write_text(json.dumps(self.manifest), encoding="utf-8")
        exact_hits = [
            {"doc_id": 1, "score": 1.0},
            {"doc_id": 2, "score": 0.0},
            {"doc_id": 3, "score": -1.0},
        ]
        exact = [
            {
                "candidate_k": k,
                "results": [{"query_id": 0, "hits": copy.deepcopy(exact_hits[:k])}],
            }
            for k in (2, 3)
        ]
        # Independent literal sigmoid values: fixed logits 0.5,-0.5,-1.5;
        # pool logits 2,-2 or 1.6875,-0.5625,-2.8125, from the two declared pools.
        fixed = [0.6224593312018546, 0.3775406687981454, 0.18242552380635635]
        pools = {
            2: [0.8807970779778823, 0.11920292202211755],
            3: [0.8438951025545426, 0.3629692055196168, 0.05665242530797385],
        }
        cases = []
        for seed, search, k in itertools.product((7, 42), (2, 4), (2, 3)):
            hits = [
                {
                    **hit,
                    "fixed_probability": fixed[index],
                    "pool_probability": pools[k][index],
                }
                for index, hit in enumerate(exact_hits[:k])
            ]
            cases.append(
                {
                    "seed": seed,
                    "search_list_size": search,
                    "candidate_k": k,
                    "diagnostic": {
                        "route": "approximate",
                        "requested_k": k,
                        "returned_documents": k,
                        "exact_vectors": 0,
                        "pq_estimates": 3,
                        "generation": 1,
                    },
                    "results": [{"query_id": 0, "hits": hits}],
                }
            )
        self.observations = {
            "schema_version": 3,
            "mode": "correctness",
            "suite": copy.deepcopy(suite),
            "manifest_sha256": hashlib.sha256(
                self.manifest_path.read_bytes()
            ).hexdigest(),
            "executable_sha256": "a" * 64,
            "fixtures": [{"name": "literal", "exact": exact, "cases": cases}],
        }

    def report(self, observations=None, criterion=None):
        return self.checker.build_report(
            self.manifest,
            self.observations if observations is None else observations,
            criterion,
            self.manifest_path,
        )

    def test_correctness_checks_tensor_maxima_probabilities_and_sensitivity_without_timing(
        self,
    ):
        report = self.report()
        self.assertTrue(report["passed"])
        self.assertNotIn("performance", report)
        self.assertNotIn("construction", report)
        self.assertFalse(report["empirical_calibration"])
        fixture = report["fixtures"][0]
        self.assertEqual(len(fixture["quality"]), 8)
        self.assertEqual(len(fixture["seed_variation"]), 4)
        self.assertGreater(
            fixture["probability_sensitivity"]["candidate_k"][
                "max_pool_probability_shift"
            ],
            0.2,
        )
        self.assertEqual(
            fixture["probability_sensitivity"]["candidate_k"][
                "max_fixed_probability_shift"
            ],
            0,
        )

    def test_rejects_missing_duplicate_cases_queries_hits_and_diagnostics(self):
        def missing_case(obs):
            obs["fixtures"][0]["cases"].pop()

        def duplicate_case(obs):
            obs["fixtures"][0]["cases"].append(
                copy.deepcopy(obs["fixtures"][0]["cases"][0])
            )

        def missing_exact(obs):
            obs["fixtures"][0]["exact"].pop()

        def missing_fixture(obs):
            obs["fixtures"].clear()

        def missing_query(obs):
            obs["fixtures"][0]["cases"][0]["results"].clear()

        def missing_hit(obs):
            obs["fixtures"][0]["cases"][0]["results"][0]["hits"].pop()

        def missing_probability(obs):
            del obs["fixtures"][0]["cases"][0]["results"][0]["hits"][0][
                "fixed_probability"
            ]

        def exact_fallback(obs):
            obs["fixtures"][0]["cases"][0]["diagnostic"]["route"] = "exact zero norm"

        def no_pq(obs):
            obs["fixtures"][0]["cases"][0]["diagnostic"]["pq_estimates"] = 0

        def no_generation(obs):
            del obs["fixtures"][0]["cases"][0]["diagnostic"]["generation"]

        def changed_seed(obs):
            obs["fixtures"][0]["cases"][0]["seed"] = 123

        def boolean_k(obs):
            obs["fixtures"][0]["cases"][0]["candidate_k"] = True

        for mutate in (
            missing_case,
            duplicate_case,
            missing_exact,
            missing_fixture,
            missing_query,
            missing_hit,
            missing_probability,
            exact_fallback,
            no_pq,
            no_generation,
            changed_seed,
            boolean_k,
        ):
            with self.subTest(case=mutate.__name__):
                changed = copy.deepcopy(self.observations)
                mutate(changed)
                with self.assertRaises(self.checker.BenchmarkError):
                    self.report(changed)

    def test_rejects_contract_hash_timing_or_empirical_claim_drift(self):
        for field, value in (
            ("manifest_sha256", "b" * 64),
            ("executable_sha256", "unknown"),
            ("elapsed_nanoseconds", 1),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.observations)
                changed[field] = value
                with self.assertRaises(self.checker.BenchmarkError):
                    self.report(changed)
        changed = copy.deepcopy(self.observations)
        changed["suite"]["empirical_calibration"] = True
        with self.assertRaises(self.checker.BenchmarkError):
            self.report(changed)
        with self.assertRaisesRegex(self.checker.BenchmarkError, "without Criterion"):
            self.report(criterion=pathlib.Path(self.directory.name))

    def test_independent_oracle_rejects_shared_wrong_sql_expectations(self):
        changed = copy.deepcopy(self.observations)
        # Both paths agree on an incorrect tensor maximum. Agreement is insufficient.
        for case in [
            *changed["fixtures"][0]["exact"],
            *changed["fixtures"][0]["cases"],
        ]:
            case["results"][0]["hits"][0]["score"] = 0.5
        with self.assertRaisesRegex(
            self.checker.BenchmarkError, "independent vector/tensor"
        ):
            self.report(changed)
        changed = copy.deepcopy(self.observations)
        changed["fixtures"][0]["exact"][0]["results"][0]["hits"] = [
            {"doc_id": 2, "score": 0.0},
            {"doc_id": 3, "score": -1.0},
        ]
        with self.assertRaisesRegex(self.checker.BenchmarkError, "omits"):
            self.report(changed)

    def test_probability_or_ann_score_error_fails_quality_gates(self):
        for field, value in (
            ("pool_probability", 0.5),
            ("fixed_probability", 0.5),
            ("score", 0.9),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.observations)
                changed["fixtures"][0]["cases"][0]["results"][0]["hits"][0][
                    field
                ] = value
                self.assertFalse(self.report(changed)["passed"])

    def test_canonical_ties_are_not_waived_by_tie_aware_recall(self):
        parsed = {0: [(2, 0.0)]}
        oracle = {0: {1: 0.0, 2: 0.0}}
        with self.assertRaisesRegex(self.checker.BenchmarkError, "canonical tied"):
            self.checker.validate_oracle_hits(parsed, oracle, exact=True)

    def test_frozen_decoder_requires_independent_manifest_and_binary_identities(self):
        root = pathlib.Path(self.directory.name)
        data = struct.pack("<4f", 1.0, 0.0, -1.0, 0.0)
        artifact = {
            "path": "vectors.f32",
            "rows": 2,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
        (root / "vectors.f32").write_bytes(data)
        frozen = {
            "dimensions": 2,
            "artifacts": {"corpus": artifact, "queries": artifact},
        }
        path = root / "fixture.json"
        path.write_text(json.dumps(frozen), encoding="utf-8")
        spec = {
            "generator": "frozen-f32-v1",
            "dimensions": 2,
            "corpus_size": 2,
            "query_count": 2,
            "fixture_manifest": "fixture.json",
            "fixture_manifest_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
        self.assertEqual(
            self.checker.correctness_vectors(spec, root),
            ([[[1.0, 0.0]], [[-1.0, 0.0]]], [[1.0, 0.0], [-1.0, 0.0]]),
        )
        workload = root / "workload"
        workload.mkdir()
        for manifest_path in ("../fixture.json", str(path)):
            with self.subTest(manifest_path=manifest_path):
                with self.assertRaisesRegex(self.checker.BenchmarkError, "escapes"):
                    self.checker.correctness_vectors(
                        {**spec, "fixture_manifest": manifest_path}, workload
                    )
        (root / "vectors.f32").write_bytes(data[:-1])
        with self.assertRaisesRegex(self.checker.BenchmarkError, "bytes or hash"):
            self.checker.correctness_vectors(spec, root)
        path.write_text(json.dumps({**frozen, "dimensions": 3}), encoding="utf-8")
        with self.assertRaisesRegex(self.checker.BenchmarkError, "manifest hash"):
            self.checker.correctness_vectors(spec, root)

    def test_manifest_cannot_omit_mandatory_quality_gates(self):
        del self.manifest["correctness"]["fixtures"][0]["maximum_quality"][
            "max_oracle_score_abs_error"
        ]
        self.observations["suite"] = copy.deepcopy(self.manifest["correctness"])
        self.manifest_path.write_text(json.dumps(self.manifest), encoding="utf-8")
        self.observations["manifest_sha256"] = hashlib.sha256(
            self.manifest_path.read_bytes()
        ).hexdigest()
        with self.assertRaisesRegex(
            self.checker.BenchmarkError, "required quality gate"
        ):
            self.report()


if __name__ == "__main__":
    unittest.main()
