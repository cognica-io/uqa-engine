#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import hashlib
import importlib.util
import json
import pathlib
import sys
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_cancellation", ROOT / "scripts/run-nori-cancellation-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture():
    analysis = json.loads(benchmark.common.LIMITS.read_text())
    report = {
        "schema_version": 2, "owner": "uqa-analysis", "purpose": "cooperative_cancellation",
        "target_os": "linux", "target_arch": "x86_64", "pointer_bits": 64, "threads": 1,
        "protocol": dict(benchmark.PROTOCOL), "memory_limit_bytes": 256 * 1024 * 1024,
        "unrelated_reservation_bytes": 4096, "timing_scope": "clocked cooperative cancellation return",
        "allocation_scope": "cancelled operation", "measurements": [],
        "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {}, "flags_sha256": "1" * 64,
                       "node": None, "emcc": None, "benchmark_sha256": "2" * 64, "arguments": ["--cancellation"]},
    }
    for key in ("bundle_bytes", "bundle_sha256", "corpus_sha256"):
        report[key] = analysis[key]
    for name, output in benchmark.expected_outputs().items():
        for point, at in (("first", 1), ("middle", 6), ("last", 12)):
            report["measurements"].append({
                "name": f"{name}/{point}", **output, "complete_polls": 12, "cancel_at_poll": at,
                "verified_cancellations": 115, "verified_recoveries": 10, "remaining_budget_bytes": 4096,
                "sample_wall_ns": [75_000_000] * 7,
                "allocation": {"count_total": 8, "count_peak": 8, "count_retained": 0,
                               "bytes_total": 64, "bytes_peak": 64, "bytes_retained": 0},
                "operation_timing": {"elapsed_ns": [1024] * 7, "iterations": [16] * 7, "median_ns": 64.0},
                "response_timing": {"elapsed_ns": [512] * 7, "iterations": [16] * 7, "median_ns": 32.0},
            })
    limits = {"schema_version": 1, "timing_max_ratio": 1.2,
              "outputs": {row["name"]: {key: row[key] for key in benchmark.OUTPUT_KEYS} for row in report["measurements"]},
              "allocation_ceilings": {"linux/x86_64/64": {row["name"]: dict(row["allocation"]) for row in report["measurements"]}}}
    for key in ("bundle_bytes", "bundle_sha256", "corpus_sha256"):
        limits[key] = report[key]
    return report, limits


def fixed_fixture():
    pilot, limits = fixture()
    pilot["provenance"].update(cargo_lock_sha256="3" * 64, runtime_sources_sha256="4" * 64,
                               artifacts=[{"name": "nori-0123456789abcdef", "bytes": 16, "sha256": "5" * 64}])
    report = copy.deepcopy(pilot)
    plan = {row["name"]: 32 for row in pilot["measurements"]}
    argument = json.dumps(plan, sort_keys=True, separators=(",", ":"))
    report.update(schema_version=3, sampling_iterations=plan, sampling_pilot=pilot)
    report["protocol"]["fixed_iterations_sha256"] = hashlib.sha256(argument.encode()).hexdigest()
    report["provenance"]["arguments"] = ["--cancellation", "--cancellation-fixed-iterations", argument]
    for row in report["measurements"]:
        for scope in benchmark.TIMINGS:
            row[scope]["iterations"] = [32] * 7
            row[scope]["elapsed_ns"] = [value * 2 for value in row[scope]["elapsed_ns"]]
        row["verified_cancellations"] = 227
    return report, limits


class NoriCancellationBenchmarkTests(unittest.TestCase):
    def test_fixed_sampling_preserves_every_existing_gate(self):
        report, limits = fixed_fixture()
        self.assertEqual(len(benchmark.measurements(report)), 108)
        result = benchmark.check(report, limits, copy.deepcopy(report))
        self.assertTrue(result["allocation_and_recovery_passed"])
        self.assertEqual(set(result["timing_ratios"].values()), {1.0})
        changed = copy.deepcopy(report)
        changed["measurements"][0]["allocation"]["bytes_total"] += 1
        with self.assertRaisesRegex(RuntimeError, "allocation regression"):
            benchmark.check(changed, limits)

    def test_fixed_plan_requires_complete_names_valid_counts_and_exact_identity(self):
        for mutate in (lambda plan: plan.pop(next(iter(plan))),
                       lambda plan: plan.update({next(iter(plan)): True}),
                       lambda plan: plan.update({next(iter(plan)): 33})):
            report, _ = fixed_fixture()
            mutate(report["sampling_iterations"])
            with self.assertRaises(RuntimeError):
                benchmark.measurements(report)
        report, _ = fixed_fixture()
        report["protocol"]["fixed_iterations_sha256"] = "6" * 64
        with self.assertRaisesRegex(RuntimeError, "sampling protocol"):
            benchmark.measurements(report)
        report, _ = fixed_fixture()
        name = next(iter(report["sampling_iterations"]))
        report["sampling_iterations"][name] = 64
        argument = json.dumps(report["sampling_iterations"], sort_keys=True, separators=(",", ":"))
        report["protocol"]["fixed_iterations_sha256"] = hashlib.sha256(argument.encode()).hexdigest()
        report["provenance"]["arguments"][-1] = argument
        with self.assertRaisesRegex(RuntimeError, "differs from its pilot"):
            benchmark.measurements(report)

    def test_fixed_samples_cannot_change_work_even_with_consistent_timing_totals(self):
        report, _ = fixed_fixture()
        row = report["measurements"][0]
        for scope in benchmark.TIMINGS:
            row[scope]["iterations"][0] = 48
            row[scope]["elapsed_ns"][0] = int(row[scope]["median_ns"] * 48)
        row["verified_cancellations"] += 16
        with self.assertRaisesRegex(RuntimeError, "sample differs from its fixed iteration plan"):
            benchmark.measurements(report)

    def test_fixed_pilot_and_invocation_must_match_the_measured_environment(self):
        for change in (lambda report: report.pop("sampling_pilot"),
                       lambda report: report["sampling_pilot"].update(schema_version=3),
                       lambda report: report["sampling_pilot"]["provenance"].update(cpu="different CPU"),
                       lambda report: report["provenance"].update(arguments=["--cancellation"])):
            report, _ = fixed_fixture()
            change(report)
            with self.assertRaises(RuntimeError):
                benchmark.measurements(report)

    def test_new_measurement_uses_a_fresh_process_and_repeat_reuses_the_same_plan(self):
        report, _ = fixed_fixture()
        pilot = report["sampling_pilot"]
        bare = copy.deepcopy(report)
        bare.pop("sampling_pilot")
        with patch.object(benchmark.common, "execute_benchmark", side_effect=[copy.deepcopy(pilot), copy.deepcopy(bare)]) as execute:
            first = benchmark.run("native")
        self.assertEqual(execute.call_count, 2)
        self.assertEqual(execute.call_args_list[0].kwargs["arguments"], ("--cancellation",))
        self.assertEqual(execute.call_args_list[1].kwargs["arguments"], tuple(report["provenance"]["arguments"]))
        with patch.object(benchmark.common, "execute_benchmark", return_value=copy.deepcopy(bare)) as execute:
            second = benchmark.run("native", first)
        execute.assert_called_once()
        self.assertEqual(execute.call_args.kwargs["arguments"], tuple(report["provenance"]["arguments"]))
        self.assertEqual(first["sampling_iterations"], second["sampling_iterations"])
        self.assertEqual(first["sampling_pilot"], second["sampling_pilot"])

    def test_collection_rejects_an_adaptive_baseline_or_changed_pilot_executable(self):
        legacy, _ = fixture()
        with patch.object(benchmark.common, "execute_benchmark") as execute:
            with self.assertRaisesRegex(RuntimeError, "baseline with a shared iteration plan"):
                benchmark.run("native", legacy)
            execute.assert_not_called()
        report, _ = fixed_fixture()
        pilot = report.pop("sampling_pilot")
        report["provenance"]["artifacts"][0]["sha256"] = "7" * 64
        with patch.object(benchmark.common, "execute_benchmark", side_effect=[pilot, report]):
            with self.assertRaisesRegex(RuntimeError, "executable changed after selecting"):
                benchmark.run("native")

    def test_ci_requires_allocation_recovery_and_both_timing_scopes(self):
        workflow = (ROOT / ".github/workflows/nori-cancellation-benchmarks.yml").read_text()
        self.assertIn("target: [native, wasm]", workflow)
        self.assertNotIn("--measure-only", workflow)
        self.assertIn('run-nori-cancellation-benchmark.py --target "$BENCHMARK_TARGET" --baseline', workflow)
        parent = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("uses: ./.github/workflows/nori-cancellation-benchmarks.yml", parent)

    def test_complete_corpus_modes_stages_and_points_are_checked(self):
        report, limits = fixture()
        self.assertEqual(len(benchmark.measurements(report)), 108)
        result = benchmark.check(report, limits, copy.deepcopy(report))
        self.assertTrue(result["allocation_and_recovery_passed"])
        self.assertTrue(result["timing_compared"])
        self.assertEqual(len(result["timing_ratios"]), 216)
        self.assertEqual(set(result["timing_ratios"].values()), {1.0})
        self.assertFalse(benchmark.check(report, limits)["timing_compared"])

    def test_partial_duplicate_and_unknown_workloads_fail(self):
        for mutate in (lambda rows: rows.pop(), lambda rows: rows.append(rows[0]), lambda rows: rows[0].update(name="unknown")):
            report, _ = fixture(); mutate(report["measurements"])
            with self.assertRaisesRegex(RuntimeError, "missing, duplicate, or unknown"):
                benchmark.measurements(report)

    def test_complete_inputs_and_recovered_graphs_remain_the_reviewed_contract(self):
        for key, value in (("input_sha256", "changed"), ("input_bytes", 0), ("input_utf16", 0), ("output_sha256", "changed"), ("tokens", 0)):
            report, _ = fixture(); report["measurements"][0][key] = value
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "complete recovery output or input"):
                benchmark.measurements(report)

    def test_cancel_points_cleanup_and_every_recovery_are_required(self):
        for key, value in (("complete_polls", 2), ("cancel_at_poll", 2), ("verified_cancellations", 114), ("verified_recoveries", 9), ("remaining_budget_bytes", 0)):
            report, _ = fixture(); report["measurements"][0][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError): benchmark.measurements(report)
        report, _ = fixture(); report["measurements"][0]["complete_polls"] = 14
        with self.assertRaisesRegex(RuntimeError, "inconsistent cancellation poll count"):
            benchmark.measurements(report)

        report, _ = fixture()
        for row, at in zip(report["measurements"][:3], (1, 2, 3)):
            row.update(complete_polls=3, cancel_at_poll=at)
        self.assertEqual(len(benchmark.measurements(report)), 108)

    def test_retained_allocations_and_every_one_unit_regression_fail(self):
        for key in benchmark.common.ALLOCATION_KEYS:
            report, limits = fixture(); row = report["measurements"][0]
            row["allocation"][key] += 1
            with self.subTest(key=key), self.assertRaises(RuntimeError): benchmark.check(report, limits)
        report, limits = fixture(); del limits["allocation_ceilings"]["linux/x86_64/64"][report["measurements"][0]["name"]]["bytes_total"]
        with self.assertRaisesRegex(RuntimeError, "incomplete cancellation allocation"):
            benchmark.check(report, limits)

    def test_short_forged_and_impossible_timing_samples_fail(self):
        for key in benchmark.TIMINGS:
            for change in ({"elapsed_ns": [512] * 6}, {"elapsed_ns": [True] * 7}, {"iterations": 15}, {"median_ns": float("nan")}, {"median_ns": 1}):
                report, _ = fixture(); report["measurements"][0][key].update(change)
                with self.subTest(key=key, change=change), self.assertRaises(RuntimeError): benchmark.measurements(report)
        report, _ = fixture(); report["measurements"][0]["response_timing"].update(elapsed_ns=[2048] * 7, median_ns=128)
        with self.assertRaisesRegex(RuntimeError, "inconsistent cancellation return"):
            benchmark.measurements(report)

    def test_zero_response_observations_do_not_claim_a_timing_ratio(self):
        report, limits = fixture(); report["measurements"][0]["response_timing"].update(elapsed_ns=[0] * 7, median_ns=0)
        self.assertTrue(benchmark.check(report, limits)["allocation_and_recovery_passed"])
        with self.assertRaisesRegex(RuntimeError, "clock resolution"):
            benchmark.check(report, limits, copy.deepcopy(report))

    def test_short_samples_and_incomplete_adaptive_counts_fail(self):
        for duration in (74_999_999, True, -1):
            report, _ = fixture(); report["measurements"][0]["sample_wall_ns"][0] = duration
            with self.subTest(duration=duration), self.assertRaisesRegex(RuntimeError, "sample duration"):
                benchmark.measurements(report)
        for count in (0, 15, 17, True):
            report, _ = fixture(); report["measurements"][0]["operation_timing"]["iterations"][0] = count
            with self.subTest(count=count), self.assertRaisesRegex(RuntimeError, "operation counts"):
                benchmark.measurements(report)
        report, _ = fixture(); row = report["measurements"][0]
        for key in benchmark.TIMINGS:
            row[key]["iterations"] = [16, 32, 48, 64, 80, 96, 112]
            row[key]["elapsed_ns"] = [count * row[key]["median_ns"] for count in row[key]["iterations"]]
            row[key]["elapsed_ns"] = list(map(int, row[key]["elapsed_ns"]))
        row["verified_cancellations"] = 3 + sum(row["operation_timing"]["iterations"])
        self.assertEqual(len(benchmark.measurements(report)), 108)
        row["response_timing"]["iterations"][0] = 32
        with self.assertRaisesRegex(RuntimeError, "timing samples"):
            benchmark.measurements(report)

    def test_unmeasured_targets_and_incomparable_timing_environments_fail(self):
        report, limits = fixture(); report["target_os"] = "macos"
        with self.assertRaisesRegex(RuntimeError, "complete target"):
            benchmark.check(report, limits)
        for key in ("cpu", "platform", "rustc", "flags", "flags_sha256", "node", "emcc", "benchmark_sha256", "arguments"):
            report, limits = fixture(); baseline = copy.deepcopy(report); baseline["provenance"][key] = "3" * 64 if key == "flags_sha256" else "changed"
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "incomparable cancellation environment"):
                benchmark.check(report, limits, baseline)

    def test_both_operation_and_cancelled_return_timing_regressions_fail(self):
        for key in benchmark.TIMINGS:
            report, limits = fixture(); baseline = copy.deepcopy(report)
            row = report["measurements"][0][key]
            row.update(elapsed_ns=[value * 2 for value in row["elapsed_ns"]], median_ns=row["median_ns"] * 2)
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "cancellation timing regression"):
                benchmark.check(report, limits, baseline)

    def test_measured_and_reviewed_sources_cannot_be_overwritten(self):
        for path in (benchmark.BENCHMARK, benchmark.SUPPORT, benchmark.LIMITS, benchmark.common.LIMITS, benchmark.common.CORPUS):
            with self.subTest(path=path), patch.object(sys, "argv", ["run", "--measure-only", "--output", str(path)]), patch.object(benchmark.common, "execute_benchmark") as execute:
                with self.assertRaises(SystemExit) as error: benchmark.main()
                self.assertEqual(error.exception.code, 2)
                execute.assert_not_called()


if __name__ == "__main__":
    unittest.main()
