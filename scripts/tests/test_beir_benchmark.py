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
import stat
import tempfile
import unittest
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_script(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BeirBenchmarkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.checker = load_script(
            "check_beir_benchmark", ROOT / "scripts" / "check-beir-benchmark.py"
        )
        cls.preparer = load_script(
            "prepare_beir_benchmark", ROOT / "scripts" / "prepare-beir-benchmark.py"
        )

    def test_quality_metrics_use_full_qrels_ideal_and_recall(self) -> None:
        perfect = self.checker.quality_metrics(
            {"q1": {1: 3.0, 2: 1.0}, "q2": {3: 1.0}},
            {"q1": [1, 2], "q2": [3]},
            2,
        )
        self.assertEqual(perfect["ndcg_at_2"], 1.0)
        self.assertEqual(perfect["map_at_2"], 1.0)
        self.assertEqual(perfect["recall_at_2"], 1.0)
        self.assertEqual(perfect["mrr_at_2"], 1.0)

        incomplete = self.checker.quality_metrics(
            {"q1": {1: 3.0, 2: 1.0}}, {"q1": [1, 3]}, 2
        )
        self.assertLess(incomplete["ndcg_at_2"], 1.0)
        self.assertEqual(incomplete["map_at_2"], 0.5)
        self.assertEqual(incomplete["recall_at_2"], 0.5)

    def test_report_requires_persistent_sql_and_checks_hybrid_comparison(self) -> None:
        with tempfile.TemporaryDirectory() as directory_name:
            directory = pathlib.Path(directory_name)
            manifest, observations = self.payloads()
            manifest_path = directory / "manifest.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            observations["benchmark_manifest_sha256"] = self.preparer.manifest_sha256(
                manifest_path
            )
            report = self.checker.build_report(
                manifest, observations, None, manifest_path
            )
            self.assertTrue(report["passed"])
            self.assertEqual(report["quality"]["hybrid"]["metrics"]["ndcg_at_2"], 1.0)

            observations["storage"] = {
                "backend": "memory",
                "persistent": False,
                "reopened_before_query_phase": False,
            }
            with self.assertRaisesRegex(self.checker.BenchmarkError, "storage identity"):
                self.checker.build_report(manifest, observations, None, manifest_path)

    def test_rank_parser_rejects_duplicate_and_misordered_hits(self) -> None:
        specification = {"name": "hybrid", "score_domain": "probability"}
        duplicate = {
            "results": [
                {
                    "query_id": "q1",
                    "hits": [
                        {"doc_id": 1, "score": 0.9},
                        {"doc_id": 1, "score": 0.8},
                    ],
                }
            ]
        }
        with self.assertRaisesRegex(self.checker.BenchmarkError, "repeats doc_id"):
            self.checker.parse_system_results(duplicate, specification, {"q1"}, 2)

        misordered = {
            "results": [
                {
                    "query_id": "q1",
                    "hits": [
                        {"doc_id": 1, "score": 0.7},
                        {"doc_id": 2, "score": 0.8},
                    ],
                }
            ]
        }
        with self.assertRaisesRegex(self.checker.BenchmarkError, "rank order"):
            self.checker.parse_system_results(misordered, specification, {"q1"}, 2)

    def test_prepared_cache_depends_on_data_and_embedding_identity_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory_name:
            output = pathlib.Path(directory_name)
            artifact = output / "corpus.jsonl"
            artifact.write_text("{}\n", encoding="utf-8")
            identity = {"name": "fixture"}
            embedding = {"model": "fixture-model"}
            prepared = {
                "dataset": identity,
                "embedding": embedding,
                "artifacts": {
                    "corpus": {
                        "path": artifact.name,
                        "sha256": self.preparer.sha256_file(artifact),
                    }
                },
            }
            (output / "prepared-manifest.json").write_text(
                json.dumps(prepared), encoding="utf-8"
            )
            self.assertTrue(
                self.preparer.prepared_output_is_current(output, identity, embedding)
            )
            self.assertFalse(
                self.preparer.prepared_output_is_current(
                    output, identity, {"model": "different"}
                )
            )

    def test_zip_validation_rejects_traversal_and_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as directory_name:
            directory = pathlib.Path(directory_name)
            traversal_path = directory / "traversal.zip"
            with zipfile.ZipFile(traversal_path, "w") as archive:
                archive.writestr("../escape", "bad")
            with zipfile.ZipFile(traversal_path) as archive:
                with self.assertRaisesRegex(self.preparer.PreparationError, "unsafe"):
                    self.preparer.validate_zip_members(archive)

            symlink_path = directory / "symlink.zip"
            member = zipfile.ZipInfo("dataset/link")
            member.external_attr = (stat.S_IFLNK | 0o777) << 16
            with zipfile.ZipFile(symlink_path, "w") as archive:
                archive.writestr(member, "target")
            with zipfile.ZipFile(symlink_path) as archive:
                with self.assertRaisesRegex(self.preparer.PreparationError, "symlink"):
                    self.preparer.validate_zip_members(archive)

    def test_extraction_cache_is_scoped_by_archive_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory_name:
            directory = pathlib.Path(directory_name)
            cache = directory / "cache"
            extracted = []
            for archive_name, marker in (
                ("fixture-aaaa.zip", "first"),
                ("fixture-bbbb.zip", "second"),
            ):
                archive_path = directory / archive_name
                with zipfile.ZipFile(archive_path, "w") as archive:
                    archive.writestr("fixture/corpus.jsonl", marker)
                    archive.writestr("fixture/queries.jsonl", "query")
                    archive.writestr("fixture/qrels/test.tsv", "qrels")
                path = self.preparer.extract_dataset(archive_path, cache, "fixture")
                extracted.append(path)
                self.assertEqual((path / "corpus.jsonl").read_text(), marker)
            self.assertNotEqual(extracted[0], extracted[1])

    @staticmethod
    def payloads():
        storage = {
            "backend": "sqlite",
            "persistent": True,
            "reopened_before_query_phase": True,
        }
        execution = {
            "api": "Engine::sql",
            "load": "SQL load",
            "index_lifecycle": "SQL CREATE INDEX",
            "text_query": "text sql",
            "vector_query": "vector sql",
            "hybrid_query": "hybrid sql",
        }
        systems = [
            {
                "name": name,
                "query": f"{name}_query",
                "score_domain": domain,
                "require_top_k": True,
                "criterion_benchmark": f"group/fixture/{name}",
                "minimum_quality": {"ndcg_at_2": 0.0},
            }
            for name, domain in (
                ("text", "nonnegative"),
                ("vector", "cosine"),
                ("hybrid", "probability"),
            )
        ]
        manifest = {
            "schema_version": 1,
            "dataset": {"expected_corpus_count": 3, "expected_query_count": 1},
            "embedding": {"model": "fixture"},
            "storage": storage,
            "execution": execution,
            "indexes": [
                {"name": "sql_create_gin", "statement": "CREATE GIN"},
                {"name": "sql_create_hnsw", "statement": "CREATE HNSW"},
            ],
            "workload": {
                "top_k": 2,
                "quality_query_count": 1,
                "performance_query_count": 1,
            },
            "systems": systems,
            "comparative_quality": [
                {
                    "name": "hybrid_vs_single",
                    "system": "hybrid",
                    "metric": "ndcg_at_2",
                    "baseline_systems": ["text", "vector"],
                    "minimum_delta": 0.0,
                }
            ],
            "measurement": {"criterion_point_estimator": "mean"},
        }
        system_results = [
            {
                "name": "text",
                "results": [
                    {
                        "query_id": "q1",
                        "hits": [
                            {"doc_id": 1, "score": 2.0},
                            {"doc_id": 3, "score": 1.0},
                        ],
                    }
                ],
            },
            {
                "name": "vector",
                "results": [
                    {
                        "query_id": "q1",
                        "hits": [
                            {"doc_id": 1, "score": 0.9},
                            {"doc_id": 3, "score": 0.8},
                        ],
                    }
                ],
            },
            {
                "name": "hybrid",
                "results": [
                    {
                        "query_id": "q1",
                        "hits": [
                            {"doc_id": 1, "score": 0.9},
                            {"doc_id": 2, "score": 0.8},
                        ],
                    }
                ],
            },
        ]
        observations = {
            "schema_version": 1,
            "benchmark_manifest_sha256": "pending",
            "dataset": manifest["dataset"],
            "embedding": manifest["embedding"],
            "storage": storage,
            "execution": execution,
            "indexes": manifest["indexes"],
            "workload": manifest["workload"],
            "preparation": {
                "archive_cache_hit": True,
                "download_seconds": 0.0,
                "corpus_embedding_seconds": 1.0,
                "query_embedding_seconds": 1.0,
            },
            "artifacts": {
                "corpus": {"path": "corpus.jsonl", "rows": 3, "sha256": "0" * 64},
                "queries": {"path": "queries.jsonl", "rows": 1, "sha256": "1" * 64},
            },
            "queries": [{"query_id": "q1", "judgments": {"1": 3.0, "2": 1.0}}],
            "systems": system_results,
            "construction": [
                {
                    "name": "sql_load",
                    "statement": "SQL load",
                    "rows": 3,
                    "elapsed_nanoseconds": 10,
                },
                {
                    "name": "sql_create_gin",
                    "statement": "CREATE GIN",
                    "rows": 3,
                    "elapsed_nanoseconds": 10,
                },
                {
                    "name": "sql_create_hnsw",
                    "statement": "CREATE HNSW",
                    "rows": 3,
                    "elapsed_nanoseconds": 10,
                },
            ],
        }
        return manifest, observations


if __name__ == "__main__":
    unittest.main()
