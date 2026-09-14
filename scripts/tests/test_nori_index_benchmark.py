#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_index_benchmark", ROOT / "scripts/run-nori-index-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture():
    rows = [{"name": name, "documents_after": count, "elapsed_ns": [1000] * 7, "median_ns": 1000,
             "allocation": {"count_total": 4, "count_peak": 3, "count_net": 2, "bytes_total": 40, "bytes_peak": 30, "bytes_net": 20},
             "graph_sha256": "a" * 64, "field_length": count * 2, "posting_count": count * 2}
            for name, count in benchmark.EXPECTED.items()]
    report = {"schema_version": 1, "owner": "uqa-storage", "threads": 1, "pointer_bits": 64,
              "target_arch": "aarch64", "target_os": "macos", "timing_scope": "mutation", "allocation_scope": "mutation",
              "protocol": dict(benchmark.PROTOCOL), "corpus_sha256": benchmark.common.digest(benchmark.common.CORPUS),
              "analyzer_fingerprint": "b" * 64, "measurements": rows,
              "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {}, "node": None, "emcc": None, "benchmark_sha256": "c" * 64}}
    limits = {key: report[key] for key in ("schema_version", "corpus_sha256", "analyzer_fingerprint")}
    limits.update(timing_max_ratio=1.25,
                  allocation_ceilings={"64": {row["name"]: dict(row["allocation"]) for row in rows}},
                  outputs={row["name"]: {key: row[key] for key in ("graph_sha256", "field_length", "posting_count")} for row in rows})
    return report, limits


class NoriIndexBenchmarkTest(unittest.TestCase):
    def test_reviewed_limits_cover_both_pointer_widths_and_every_workload(self):
        limits = json.loads(benchmark.LIMITS.read_text())
        self.assertEqual(set(limits["allocation_ceilings"]), {"32", "64"})
        self.assertEqual(set(limits["outputs"]), set(benchmark.EXPECTED))
        for ceilings in limits["allocation_ceilings"].values():
            self.assertEqual(set(ceilings), set(benchmark.EXPECTED))

    def test_full_mutation_contract_passes_and_timing_is_separate(self):
        report, limits = fixture()
        self.assertEqual(benchmark.check(report, limits), {"allocation_and_graph_passed": True, "timing_compared": False, "timing_ratios": {}})
        result = benchmark.check(report, limits, copy.deepcopy(report))
        self.assertEqual(set(result["timing_ratios"].values()), {1.0})

    def test_net_counters_can_include_released_seed_allocations(self):
        report, limits = fixture()
        report["measurements"][0]["allocation"].update(count_net=-2, bytes_net=-20)
        self.assertTrue(benchmark.check(report, limits)["allocation_and_graph_passed"])

    def test_missing_duplicate_and_unknown_workloads_fail(self):
        for mutation in (lambda rows: rows.pop(), lambda rows: rows.append(rows[0]), lambda rows: rows[0].update(name="unknown")):
            report, limits = fixture()
            mutation(report["measurements"])
            with self.assertRaisesRegex(RuntimeError, "missing, duplicate, or unknown"):
                benchmark.check(report, limits)

    def test_invalid_samples_and_forged_estimators_fail(self):
        for change in ({"elapsed_ns": [0] * 7}, {"elapsed_ns": [1000] * 6}, {"elapsed_ns": [True] * 7}, {"median_ns": float("nan")}, {"median_ns": 999}):
            report, limits = fixture()
            report["measurements"][0].update(change)
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_changed_graph_metadata_or_counts_fail(self):
        for key in ("graph_sha256", "field_length", "posting_count", "documents_after"):
            report, limits = fixture()
            report["measurements"][0][key] = "changed"
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_every_allocation_metric_is_gated(self):
        for key in benchmark.ALLOCATION_KEYS:
            report, limits = fixture()
            name = report["measurements"][0]["name"]
            limits["allocation_ceilings"]["64"][name][key] -= 1
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                benchmark.check(report, limits)

    def test_incomplete_and_nonfinite_limits_fail(self):
        report, limits = fixture()
        name = report["measurements"][0]["name"]
        for malformed in (None, {}, {"graph_sha256": "a" * 64}):
            changed = copy.deepcopy(limits)
            changed["outputs"][name] = malformed
            with self.assertRaisesRegex(RuntimeError, "incomplete indexing output"):
                benchmark.check(report, changed)
        limits["allocation_ceilings"]["64"][name]["bytes_peak"] = float("nan")
        with self.assertRaisesRegex(RuntimeError, "invalid indexing allocation ceiling"):
            benchmark.check(report, limits)

    def test_timing_rejects_incomparable_environment_and_graphs(self):
        report, limits = fixture()
        for key in report["provenance"]:
            baseline = copy.deepcopy(report)
            del baseline["provenance"][key]
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "incomparable"):
                benchmark.check(report, limits, baseline)
        baseline = copy.deepcopy(report)
        baseline["measurements"][0]["graph_sha256"] = "changed"
        with self.assertRaisesRegex(RuntimeError, "incomparable indexing timing outputs"):
            benchmark.check(report, limits, baseline)

    def test_timing_regressions_fail(self):
        report, limits = fixture()
        baseline = copy.deepcopy(report)
        report["measurements"][0].update(elapsed_ns=[2000] * 7, median_ns=2000)
        with self.assertRaisesRegex(RuntimeError, "indexing timing regression"):
            benchmark.check(report, limits, baseline)

    def test_ci_and_feature_configuration_require_the_storage_owner(self):
        native = (ROOT / ".github/workflows/ci.yml").read_text()
        wasm = (ROOT / ".github/workflows/javascript-bindings.yml").read_text()
        self.assertIn("run-nori-index-benchmark.py --output", native)
        self.assertIn("run-nori-index-benchmark.py --target wasm --output", wasm)
        self.assertNotIn("--measure-only", native + wasm)
        manifest = (ROOT / "crates/uqa-storage/Cargo.toml").read_text()
        self.assertIn('required-features = ["uqa-analysis/nori"]', manifest)


if __name__ == "__main__":
    unittest.main()
