#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import hashlib
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_sql_measurement", ROOT / "scripts/run-nori-sql-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture(wasm=False):
    corpus = benchmark.phrase.cases()
    providers = ["memory", "sqlite"] + ([] if wasm else ["redb"])
    fingerprints = {mode: f"{i + 1:064x}" for i, mode in enumerate(benchmark.MODES)}
    mutations = {}
    for key, count in benchmark.MUTATIONS.items():
        mutations[key] = {"documents": count, "rows_sha256": benchmark.source_hash(count),
                          "analyzer_fingerprint": fingerprints["mixed"],
                          "queries": [[[doc, 0.9] for doc in range(i, count, len(corpus))] for i in range(len(corpus))]}
    rows = []
    for name in sorted(benchmark.expected_names(providers)):
        row = {"name": name, "elapsed_ns": [1000] * 7, "median_ns": 1000, "verified_samples": 9,
               "allocation": {"count_total": 4, "count_peak": 3, "count_retained": -2,
                              "bytes_total": 40, "bytes_peak": 30, "bytes_retained": -20}}
        provider, stage, *rest = name.split("/")
        key = benchmark.output_key(name)
        if key in mutations:
            row.update(snapshot=copy.deepcopy(mutations[key]), reopened_samples=0 if provider == "memory" else 9,
                       reopened_snapshot=None if provider == "memory" else copy.deepcopy(mutations[key]))
        else:
            mode, case_name = rest
            i, case = next((i, case) for i, case in enumerate(corpus) if case["name"] == case_name)
            row.update(analyzer_fingerprint=fingerprints[mode], query_sha256=hashlib.sha256(case["text"].encode()).hexdigest(),
                       rows=[[doc, 0.9] for doc in [*range(i, benchmark.DOCUMENTS, len(corpus)), benchmark.DOCUMENTS + i]])
        rows.append(row)
    report = {"schema_version": 1, "owner": "uqa", "protocol": dict(benchmark.PROTOCOL),
              "pointer_bits": 32 if wasm else 64, "foreground_threads": 1,
              "target_arch": "wasm32" if wasm else "aarch64", "target_os": "emscripten" if wasm else "macos",
              "providers": providers, "provider_settings": {"sqlite": {"journal_mode": "delete"}},
              "query_documents": benchmark.DOCUMENTS + len(corpus), "work_mem_bytes": 256 * 1024 * 1024,
              "corpus_sha256": benchmark.common.digest(benchmark.common.CORPUS), "measurements": rows,
              "timing_scope": "SQL", "allocation_scope": "SQL", "background_statistics": "normal",
              "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {},
                             "flags_sha256": benchmark.common.flags_signature({}), "node": None, "emcc": None,
                             "benchmark_sha256": "c" * 64, "cargo_lock_sha256": "d" * 64}}
    limits = {"schema_version": 1, "corpus_sha256": report["corpus_sha256"], "timing_max_ratio": 1.2,
              "allocation_ceilings": {benchmark.target_key(report): {row["name"]: dict(row["allocation"]) for row in rows}},
              "outputs": {benchmark.output_key(row["name"]): copy.deepcopy(benchmark.output(row)) for row in rows}}
    return report, limits


class NoriSQLBenchmarkTest(unittest.TestCase):
    def test_native_wasm_complete_contract_and_signed_retention(self):
        for wasm, count in ((False, 102), (True, 62)):
            report, limits = fixture(wasm)
            self.assertEqual(len(benchmark.measurements(report)), count)
            result = benchmark.check(report, limits, copy.deepcopy(report))
            self.assertTrue(result["allocation_and_rows_passed"])
            self.assertTrue(result["timing_compared"])

    def test_missing_providers_sessions_and_sampling_fail(self):
        for change in ("provider", "session", "duplicate", "samples", "verified", "owner", "allowance", "flags"):
            report, _ = fixture()
            if change == "provider": report["providers"].pop()
            elif change == "session": report["measurements"] = [r for r in report["measurements"] if "/session_and_query/" not in r["name"]]
            elif change == "duplicate": report["measurements"].append(copy.deepcopy(report["measurements"][0]))
            elif change == "samples": report["measurements"][0]["elapsed_ns"].pop()
            elif change == "verified": report["measurements"][0]["verified_samples"] = 8
            elif change == "owner": report["owner"] = "uqa-engine"
            elif change == "allowance": report["work_mem_bytes"] *= 2
            else: del report["provenance"]["flags_sha256"]
            with self.subTest(change=change), self.assertRaises(RuntimeError): benchmark.measurements(report)

    def test_complete_live_and_reopened_state_is_required(self):
        for change in ("count", "source", "scores", "rows", "reopens", "absent"):
            report, _ = fixture()
            row = next(r for r in report["measurements"] if r["name"] == "sqlite/commit_16/256")
            if change == "count": row["reopened_snapshot"]["documents"] -= 1
            elif change == "source": row["reopened_snapshot"]["rows_sha256"] = "a" * 64
            elif change == "scores": row["reopened_snapshot"]["queries"][0][0][1] += 0.1
            elif change == "rows": row["reopened_snapshot"]["queries"][0].pop()
            elif change == "reopens": row["reopened_samples"] = 8
            else: del row["reopened_snapshot"]
            with self.subTest(change=change), self.assertRaises(RuntimeError): benchmark.measurements(report)

    def test_complete_phrase_documents_and_inputs_are_required(self):
        for change in ("missing", "control", "order", "invalid", "score", "input", "mode"):
            report, _ = fixture()
            row = next(r for r in report["measurements"] if "/sql_query/none/" in r["name"])
            if change == "missing": row["rows"].pop(0)
            elif change == "control": row["rows"].pop()
            elif change == "order": row["rows"].reverse()
            elif change == "invalid": row["rows"][0][1] = float("nan")
            elif change == "score": row["rows"][0][1] += 0.1
            elif change == "input": row["query_sha256"] = "a" * 64
            else: row["analyzer_fingerprint"] = "a" * 64
            with self.subTest(change=change), self.assertRaises(RuntimeError): benchmark.measurements(report)

    def test_unreviewed_targets_and_one_unit_regressions_fail(self):
        report, limits = fixture()
        target = benchmark.target_key(report)
        for key in benchmark.common.ALLOCATION_KEYS:
            changed = copy.deepcopy(limits)
            name = report["measurements"][0]["name"]
            changed["allocation_ceilings"][target][name][key] -= 1
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                benchmark.check(report, changed)
        del limits["allocation_ceilings"][target]
        with self.assertRaisesRegex(RuntimeError, "complete target"): benchmark.check(report, limits)

    def test_timing_requires_matching_environment_and_provider_settings(self):
        report, limits = fixture()
        for key in ("cpu", "platform", "flags_sha256", "benchmark_sha256", "cargo_lock_sha256"):
            baseline = copy.deepcopy(report)
            baseline["provenance"][key] = "f" * 64
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "timing environment"):
                benchmark.check(report, limits, baseline)
        baseline = copy.deepcopy(report)
        baseline["provider_settings"]["sqlite"]["journal_mode"] = "wal"
        with self.assertRaisesRegex(RuntimeError, "timing scope"): benchmark.check(report, limits, baseline)
        baseline = copy.deepcopy(report)
        report["measurements"][0].update(elapsed_ns=[2000] * 7, median_ns=2000)
        with self.assertRaisesRegex(RuntimeError, "timing regression"): benchmark.check(report, limits, baseline)


if __name__ == "__main__":
    unittest.main()
