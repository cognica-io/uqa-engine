#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
from contextlib import redirect_stderr
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


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
    report = {"schema_version": 3, "owner": "uqa", "protocol": dict(benchmark.PROTOCOL),
              "scheduling": {"policy": "platform_default"} if wasm else
                  {"policy": "macos_foundation_user_interactive", "requested_qos_class": 33},
              "pointer_bits": 32 if wasm else 64, "foreground_threads": 1,
              "target_arch": "wasm32" if wasm else "aarch64", "target_os": "emscripten" if wasm else "macos",
              "providers": providers, "provider_settings": {"sqlite": {"journal_mode": "delete"}},
              "catalog_inputs": benchmark.pinned_catalogs(providers),
              "query_documents": benchmark.DOCUMENTS + len(corpus), "work_mem_bytes": 256 * 1024 * 1024,
              "corpus_sha256": benchmark.common.digest(benchmark.common.CORPUS), "measurements": rows,
              "timing_scope": "SQL", "allocation_scope": "SQL", "background_statistics": "normal",
              "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {},
                             "flags_sha256": benchmark.common.flags_signature({}), "node": None, "emcc": None,
                             "benchmark_sha256": "c" * 64, "cargo_lock_sha256": "d" * 64}}
    if not wasm:
        report["provenance"]["scheduling_launcher"] = {
            "compiler": "Swift", "source_sha256": "e" * 64, "executable_sha256": "f" * 64, "child_pid": 1}
        report["provenance"]["benchmark_sources"] = {benchmark.SCHEDULING_SUPPORT.relative_to(ROOT).as_posix(): "e" * 64}
    limits = {"schema_version": 2, "corpus_sha256": report["corpus_sha256"], "timing_max_ratio": 1.2,
              "catalog_inputs": benchmark.pinned_catalogs(["sqlite", "redb"]),
              "allocation_ceilings": {benchmark.target_key(report): {row["name"]: dict(row["allocation"]) for row in rows}},
              "outputs": {benchmark.output_key(row["name"]): copy.deepcopy(benchmark.output(row)) for row in rows}}
    return report, limits


class NoriSQLBenchmarkTest(unittest.TestCase):
    def test_scheduling_is_verified_and_comparisons_require_the_same_policy(self):
        for wasm in (False, True):
            report, limits = fixture(wasm)
            for change in ("missing", "policy", "extra"):
                changed = copy.deepcopy(report)
                if change == "missing": del changed["scheduling"]
                elif change == "policy": changed["scheduling"]["policy"] = "background"
                else: changed["scheduling"]["unknown"] = True
                with self.subTest(wasm=wasm, change=change), self.assertRaisesRegex(RuntimeError, "scheduling"):
                    benchmark.measurements(changed)
            legacy = copy.deepcopy(report)
            legacy["schema_version"] = 2
            del legacy["scheduling"]
            self.assertTrue(benchmark.check(legacy, limits)["allocation_and_rows_passed"])
            with self.assertRaisesRegex(RuntimeError, "scheduling"):
                benchmark.check(report, limits, legacy)
        report, limits = fixture()
        self.assertTrue(benchmark.check(report, limits, copy.deepcopy(report))["timing_compared"])
        for key, value in (("requested_qos_class", -1), ("requested_qos_class", True),
                           ("requested_qos_class", 25), ("requested_qos_class", 33.0)):
            changed = copy.deepcopy(report)
            changed["scheduling"][key] = value
            with self.subTest(key=key, value=value), self.assertRaisesRegex(RuntimeError, "scheduling"):
                benchmark.measurements(changed)
        changed = copy.deepcopy(report)
        del changed["provenance"]["scheduling_launcher"]
        with self.assertRaisesRegex(RuntimeError, "scheduling launcher identity"):
            benchmark.measurements(changed)
        changed = copy.deepcopy(report)
        changed["provenance"]["scheduling_launcher"]["compiler"] = "another Swift compiler"
        with self.assertRaisesRegex(RuntimeError, "scheduling launcher"):
            benchmark.check(changed, limits, report)
        ordinary = copy.deepcopy(report)
        ordinary["scheduling"] = {"policy": "platform_default"}
        del ordinary["provenance"]["scheduling_launcher"]
        self.assertTrue(benchmark.check(ordinary, limits, copy.deepcopy(ordinary))["timing_compared"])
        with self.assertRaisesRegex(RuntimeError, "scheduling"):
            benchmark.check(ordinary, limits, report)

    def test_ci_checks_allocations_and_both_timing_directions(self):
        workflow = (ROOT / ".github/workflows/nori-sql-benchmarks.yml").read_text()
        self.assertIn("target: [native, wasm]", workflow)
        self.assertNotIn("--measure-only", workflow)
        self.assertIn('--baseline "target/benchmark-runs/nori-sql-$BENCHMARK_TARGET-baseline.json"', workflow)
        self.assertIn('--baseline "target/benchmark-runs/nori-sql-$BENCHMARK_TARGET-repeat.json"', workflow)
        self.assertIn('--report "target/benchmark-runs/nori-sql-$BENCHMARK_TARGET-baseline.json"', workflow)

    def test_catalog_inputs_are_required_in_reports_and_reviewed_limits(self):
        for wasm in (False, True):
            report, limits = fixture(wasm)
            self.assertTrue(benchmark.check(report, limits)["allocation_and_rows_passed"])
            for change in ("missing", "provider", "extra", "hash", "bytes", "legacy"):
                changed = copy.deepcopy(report)
                if change == "missing": del changed["catalog_inputs"]
                elif change == "provider": del changed["catalog_inputs"]["sqlite"]
                elif change == "extra": changed["catalog_inputs"]["memory"] = changed["catalog_inputs"]["sqlite"]
                elif change == "hash": changed["catalog_inputs"]["sqlite"]["sha256"] = "a" * 64
                elif change == "bytes": changed["catalog_inputs"]["sqlite"]["bytes"] += 1
                else: changed["schema_version"] = 1
                with self.subTest(wasm=wasm, change=change), self.assertRaises(RuntimeError):
                    benchmark.check(changed, limits)
            limits["catalog_inputs"]["sqlite"]["sha256"] = "a" * 64
            with self.assertRaisesRegex(RuntimeError, "catalog input identities"):
                benchmark.check(report, limits)

    def test_changed_catalog_file_is_rejected_before_building(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            manifest = directory / "manifest.json"
            manifest.write_bytes(benchmark.CATALOG_MANIFEST.read_bytes())
            for provider in ("sqlite", "redb"):
                filename = f"{provider}-empty.db"
                (directory / filename).write_bytes((benchmark.CATALOGS / filename).read_bytes())
            with patch.object(benchmark, "CATALOGS", directory), patch.object(benchmark, "CATALOG_MANIFEST", manifest):
                self.assertEqual(set(benchmark.pinned_catalogs(["sqlite", "redb"])), {"sqlite", "redb"})
                for provider in ("sqlite", "redb"):
                    path = directory / f"{provider}-empty.db"
                    original = path.read_bytes()
                    for payload in (original[:-1], b"?" + original[1:]):
                        path.write_bytes(payload)
                        with self.subTest(provider=provider, bytes=len(payload)), \
                                patch.object(benchmark.sys, "argv", ["sql-benchmark", "--output", str(directory / "result.json")]), \
                                patch.object(benchmark.common, "execute_benchmark") as execute:
                            with self.assertRaisesRegex(RuntimeError, "capture identity changed"):
                                benchmark.main()
                            execute.assert_not_called()
                    path.write_bytes(original)

    def test_seed_capture_rejects_overwrites_and_measurement_options_before_building(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            seed = directory / "capture"
            basic = ["sql-benchmark", "--capture-empty-seeds", str(seed)]
            invalid = [
                ["--output", str(seed)],
                ["--output", str(seed / "sqlite-empty.db")],
                ["--output", str(seed / "redb-empty.db")],
                ["--output", str(benchmark.BENCHMARK)],
                ["--output", str(directory / "report.json"), "--target", "wasm"],
                ["--output", str(directory / "report.json"), "--measure-only"],
                ["--output", str(directory / "report.json"), "--report", "existing.json"],
                ["--output", str(directory / "report.json"), "--baseline", "existing.json"],
                ["--output", str(directory / "report.json"), "--transaction-probe", "sqlite"],
                ["--output", str(directory / "report.json"), "--empty-seeds", str(directory)],
            ]
            for options in invalid:
                with self.subTest(options=options), patch.object(benchmark.sys, "argv", basic + options), \
                        patch.object(benchmark.common, "execute_benchmark") as execute, redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as error:
                        benchmark.main()
                    self.assertEqual(error.exception.code, 2)
                    execute.assert_not_called()
            seed.mkdir()
            marker = seed / "keep"
            marker.write_text("existing capture")
            with patch.object(benchmark.sys, "argv", basic + ["--output", str(directory / "report.json")]), \
                    patch.object(benchmark.common, "execute_benchmark") as execute, redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    benchmark.main()
                self.assertEqual(error.exception.code, 2)
                execute.assert_not_called()
            self.assertEqual(marker.read_text(), "existing capture")

    def test_transaction_experiments_require_complete_results_without_claiming_a_full_gate(self):
        source, _ = fixture()
        for provider in ("sqlite", "redb"):
            probe = {
                "schema_version": 1, "owner": "uqa", "purpose": "transaction_fixture_probe", "provider": provider,
                "corpus_sha256": source["corpus_sha256"],
                "protocol": {"samples": 7, "warmup": 1, "allocation_samples": 1},
                "empty_seed": {"bytes": 4096, "sha256": "d" * 64},
                "gate": {"allocation_and_rows_passed": False, "timing_compared": False},
                "measurements": [row for row in source["measurements"]
                                 if row["name"].startswith(provider + "/") and benchmark.output_key(row["name"]) in benchmark.MUTATIONS],
            }
            self.assertEqual(len(benchmark.transaction_probe(probe)), 4)
            with self.assertRaises(RuntimeError):
                benchmark.measurements(probe)
            for change in ("missing", "duplicate", "rows", "reopened", "samples", "counters", "analyzer", "seed", "corpus", "gate"):
                changed = copy.deepcopy(probe)
                row = changed["measurements"][0]
                if change == "missing": changed["measurements"].pop()
                elif change == "duplicate": changed["measurements"].append(row)
                elif change == "rows": row["snapshot"]["rows_sha256"] = "e" * 64
                elif change == "reopened": row["reopened_samples"] = 8
                elif change == "samples": row["elapsed_ns"].pop()
                elif change == "counters": row["allocation"]["count_total"] = -1
                elif change == "analyzer":
                    row["snapshot"]["analyzer_fingerprint"] = "e" * 64
                    row["reopened_snapshot"]["analyzer_fingerprint"] = "e" * 64
                elif change == "seed": changed["empty_seed"]["sha256"] = "invalid"
                elif change == "corpus": changed["corpus_sha256"] = "e" * 64
                else: changed["gate"]["allocation_and_rows_passed"] = True
                with self.subTest(provider=provider, change=change), self.assertRaises(RuntimeError):
                    benchmark.transaction_probe(changed)

    def test_native_wasm_complete_contract_and_signed_retention(self):
        for wasm, count in ((False, 102), (True, 62)):
            report, limits = fixture(wasm)
            self.assertEqual(len(benchmark.measurements(report)), count)
            result = benchmark.check(report, limits, copy.deepcopy(report))
            self.assertTrue(result["allocation_and_rows_passed"])
            self.assertTrue(result["timing_compared"])

    def test_negative_retention_requires_no_growth_instead_of_freeing_seed_allocations(self):
        for wasm in (False, True):
            for key in ("count_retained", "bytes_retained"):
                for value in (-1, 0, 1):
                    report, limits = fixture(wasm)
                    row = report["measurements"][0]
                    self.assertLess(limits["allocation_ceilings"][benchmark.target_key(report)][row["name"]][key], 0)
                    row["allocation"][key] = value
                    with self.subTest(wasm=wasm, counter=key, retained=value):
                        if value > 0:
                            with self.assertRaisesRegex(RuntimeError, "allocation regression"):
                                benchmark.check(report, limits)
                        else:
                            self.assertTrue(benchmark.check(report, limits)["allocation_and_rows_passed"])

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
        row = report["measurements"][0]
        name = row["name"]
        for key in ("count_retained", "bytes_retained"):
            row["allocation"][key] = abs(row["allocation"][key])
            limits["allocation_ceilings"][target][name][key] = row["allocation"][key]
        self.assertTrue(benchmark.check(report, limits)["allocation_and_rows_passed"])
        for key in benchmark.common.ALLOCATION_KEYS:
            changed = copy.deepcopy(limits)
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
