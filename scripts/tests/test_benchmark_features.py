#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from benchmark_features import feature_arguments


class BenchmarkFeatureTests(unittest.TestCase):
    def metadata(self, features=None):
        target = {"name": "analytical_comparison", "kind": ["bench"]}
        if features is not None:
            target["required-features"] = features
        return {"packages": [{"name": "uqa-engine", "targets": [target]}]}

    def test_historical_revision_keeps_default_features(self):
        self.assertEqual(feature_arguments(self.metadata(), "uqa-engine", "analytical_comparison"), [])

    def test_candidate_selects_only_its_required_features(self):
        self.assertEqual(feature_arguments(self.metadata(["duckdb"]), "uqa-engine", "analytical_comparison"),
                         ["--features", "uqa-engine/duckdb"])

    def test_missing_or_ambiguous_benchmark_is_rejected(self):
        for metadata in ({"packages": []}, {"packages": self.metadata()["packages"] * 2},
                         {"packages": [{"name": "uqa-engine", "targets": []}]}):
            with self.subTest(metadata=metadata), self.assertRaises(ValueError):
                feature_arguments(metadata, "uqa-engine", "analytical_comparison")
