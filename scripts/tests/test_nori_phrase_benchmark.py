#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_phrase_measurement", ROOT / "scripts/run-nori-phrase-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture():
    rows = []
    corpus = benchmark.cases()
    for stage in benchmark.STAGES:
        for mode_index, mode in enumerate(benchmark.MODES):
            for case_index, case in enumerate(corpus):
                ids = [*range(case_index, benchmark.DOCUMENTS, len(corpus)), benchmark.DOCUMENTS + case_index]
                rows.append({"name": f"{stage}/{mode}/{case['name']}", "query_occurrences": 1,
                             "query_sha256": hashlib.sha256(case["text"].encode()).hexdigest(),
                             "analyzer_fingerprint": f"{mode_index + 1:064x}", "rows": [[doc, 1.0] for doc in ids],
                             "elapsed_ns": [1000] * 7, "median_ns": 1000, "verified_samples": 9,
                             "allocation": {"count_total": 4, "count_peak": 3, "count_retained": 2,
                                            "bytes_total": 40, "bytes_peak": 30, "bytes_retained": 20}})
    report = {"schema_version": 1, "owner": "uqa-operators", "protocol": dict(benchmark.PROTOCOL),
              "pointer_bits": 64, "threads": 1, "target_arch": "aarch64", "target_os": "macos",
              "documents": benchmark.DOCUMENTS + len(corpus), "memory_limit": 256 * 1024 * 1024,
              "corpus_sha256": benchmark.common.digest(benchmark.common.CORPUS), "measurements": rows,
              "timing_scope": "query", "allocation_scope": "query",
              "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {},
                             "flags_sha256": benchmark.common.flags_signature({}), "node": None, "emcc": None,
                             "benchmark_sha256": "c" * 64}}
    limits = {"schema_version": 1, "corpus_sha256": report["corpus_sha256"], "timing_max_ratio": 1.2,
              "allocation_ceilings": {"64": {row["name"]: dict(row["allocation"]) for row in rows}},
              "outputs": {row["name"]: {key: copy.deepcopy(row[key]) for key in benchmark.OUTPUT_KEYS} for row in rows}}
    return report, limits


class NoriPhraseBenchmarkTest(unittest.TestCase):
    def test_allocation_only_preserves_output_and_allocation_gates_without_timing(self):
        report, limits = fixture()
        report["protocol"] = dict(benchmark.ALLOCATION_PROTOCOL)
        report.pop("timing_scope")
        for row in report["measurements"]:
            row.pop("elapsed_ns")
            row.pop("median_ns")
            row["verified_samples"] = 1
        self.assertTrue(benchmark.check(report, limits, allocation_only=True)["allocation_and_rows_passed"])
        for key, value in report["protocol"].items():
            invalid = copy.deepcopy(report)
            invalid["protocol"][key] = bool(value)
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "sampling protocol"):
                benchmark.check(invalid, limits, allocation_only=True)
        invalid = copy.deepcopy(report)
        invalid["measurements"][0]["verified_samples"] = True
        with self.assertRaisesRegex(RuntimeError, "output verification"):
            benchmark.check(invalid, limits, allocation_only=True)
        with self.assertRaisesRegex(RuntimeError, "cannot compare timing"):
            benchmark.check(report, limits, copy.deepcopy(report), allocation_only=True)
        report["measurements"][0]["allocation"]["bytes_total"] += 1
        with self.assertRaisesRegex(RuntimeError, "allocation regression"):
            benchmark.check(report, limits, allocation_only=True)

    def test_allocation_only_rejects_timing_observations(self):
        report, limits = fixture()
        report["protocol"] = dict(benchmark.ALLOCATION_PROTOCOL)
        report.pop("timing_scope")
        for row in report["measurements"]:
            row.pop("elapsed_ns")
            row.pop("median_ns")
            row["verified_samples"] = 1
        report["timing_scope"] = "uncontrolled timing"
        with self.assertRaisesRegex(RuntimeError, "timing observations"):
            benchmark.check(report, limits, allocation_only=True)

    def test_complete_phrase_contract_and_timing_comparison(self):
        report, limits = fixture()
        result = benchmark.check(report, limits, copy.deepcopy(report))
        self.assertTrue(result["allocation_and_rows_passed"])
        self.assertTrue(result["timing_compared"])

    def test_incomplete_protocol_fixture_and_modes_fail(self):
        for change in ("missing", "duplicate", "documents", "allowance", "samples", "verified", "modes", "owner", "flags"):
            report, limits = fixture()
            if change == "missing": report["measurements"].pop()
            elif change == "duplicate": report["measurements"].append(copy.deepcopy(report["measurements"][0]))
            elif change == "documents": report["documents"] -= 1
            elif change == "allowance": report["memory_limit"] += 1
            elif change == "samples": report["measurements"][0]["elapsed_ns"].pop()
            elif change == "verified": report["measurements"][0]["verified_samples"] = 8
            elif change == "owner": report["owner"] = "uqa-engine"
            elif change == "flags": del report["provenance"]["flags_sha256"]
            else:
                for row in report["measurements"]: row["analyzer_fingerprint"] = "a" * 64
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_complete_input_and_nonempty_graph_are_required(self):
        for key, value in (("query_sha256", "changed"), ("query_occurrences", 0), ("analyzer_fingerprint", "invalid")):
            report, limits = fixture()
            report["measurements"][0][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError): benchmark.check(report, limits)

    def test_false_positives_missing_matches_and_invalid_scores_fail(self):
        for change in ("wrong", "control", "missing", "duplicate", "order", "nan", "zero"):
            report, limits = fixture()
            rows = report["measurements"][0]["rows"]
            if change == "wrong": rows[0][0] = 1
            elif change == "control": rows.pop()
            elif change == "missing": rows.pop(0)
            elif change == "duplicate": rows.insert(0, rows[0][:])
            elif change == "order": rows.reverse()
            elif change == "nan": rows[0][1] = float("nan")
            else: rows[0][1] = 0
            with self.subTest(change=change), self.assertRaises(RuntimeError): benchmark.check(report, limits)

    def test_analysis_and_matching_must_retain_the_same_results(self):
        report, limits = fixture()
        report["measurements"][0]["rows"][0][1] += 0.1
        with self.assertRaisesRegex(RuntimeError, "pre-analyzed and complete"):
            benchmark.check(report, limits)

    def test_allocation_and_score_regressions_fail(self):
        report, limits = fixture()
        name = report["measurements"][0]["name"]
        for key in benchmark.ALLOCATION_KEYS:
            changed = copy.deepcopy(limits)
            changed["allocation_ceilings"]["64"][name][key] -= 1
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                benchmark.check(report, changed)
        limits["outputs"][name]["rows"][0][1] += 0.1
        with self.assertRaisesRegex(RuntimeError, "scores changed"): benchmark.check(report, limits)

    def test_timing_requires_matching_environment_and_output(self):
        report, limits = fixture()
        for key in ("cpu", "platform", "flags_sha256", "benchmark_sha256"):
            baseline = copy.deepcopy(report)
            baseline["provenance"][key] = "f" * 64 if key.endswith("_sha256") else "different"
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "timing environment"):
                benchmark.check(report, limits, baseline)
        baseline = copy.deepcopy(report)
        report["measurements"][0].update(elapsed_ns=[2000] * 7, median_ns=2000)
        with self.assertRaisesRegex(RuntimeError, "timing regression"): benchmark.check(report, limits, baseline)

    def test_roundoff_tolerance_does_not_hide_changed_documents_or_scores(self):
        self.assertTrue(benchmark.same_rows([[1, 1.0]], [[1, 1.0 + 1e-14]]))
        self.assertFalse(benchmark.same_rows([[1, 1.0]], [[2, 1.0]]))
        self.assertFalse(benchmark.same_rows([[1, 1.0]], [[1, 1.0 + 1e-9]]))

    def test_ci_enforces_native_and_wasm_phrase_gates(self):
        native = (ROOT / ".github/workflows/ci.yml").read_text()
        wasm = (ROOT / ".github/workflows/javascript-bindings.yml").read_text()
        self.assertIn("run-nori-phrase-benchmark.py --output", native)
        self.assertIn("run-nori-phrase-benchmark.py --target wasm --output", wasm)
        self.assertNotIn("--measure-only", native + wasm)


if __name__ == "__main__":
    unittest.main()
