#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import json
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "benchmarks" / "analytical" / "manifest.json"
CHECKER = ROOT / "scripts" / "check-analytical-benchmark.py"


class AnalyticalBenchmarkReportTest(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        self.benchmarks = {
            name
            for gate in self.manifest["external_ratio_checks"]
            for name in (gate["numerator"], gate["denominator"])
        } | {gate["benchmark"] for gate in self.manifest["regression_gates"]}

    def write_criterion(
        self,
        root: pathlib.Path,
        slopes: dict[str, float] | None = None,
        misleading_median: bool = False,
    ) -> None:
        slopes = slopes or {}
        for benchmark in self.benchmarks:
            estimates = root.joinpath(*benchmark.split("/"), "new", "estimates.json")
            estimates.parent.mkdir(parents=True)
            estimates.write_text(
                json.dumps(
                    {
                        "median": {
                            "point_estimate": 100.0
                            if misleading_median and benchmark.endswith("/uqa")
                            else 1.0
                        },
                        "slope": {"point_estimate": slopes.get(benchmark, 1.0)},
                    }
                ),
                encoding="utf-8",
            )

    def run_checker(
        self,
        output: pathlib.Path,
        heads: list[pathlib.Path],
        bases: list[pathlib.Path] | None = None,
        baseline_manifest: pathlib.Path = MANIFEST,
    ) -> subprocess.CompletedProcess[str]:
        command = ["python3", str(CHECKER), "--output", str(output)]
        for root in heads:
            command.extend(("--criterion-root", str(root)))
        for root in bases or []:
            command.extend(("--baseline-criterion-root", str(root)))
        if bases:
            command.extend(("--baseline-manifest", str(baseline_manifest)))
            command.extend(("--baseline-revision", "base-revision"))
        return subprocess.run(
            command,
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )

    def test_report_uses_linear_slope_without_claiming_timing_acceptance(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            criterion = root / "criterion"
            self.write_criterion(criterion, misleading_median=True)

            output = root / "report.json"
            completed = self.run_checker(output, [criterion])

            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], 5)
            self.assertFalse(report["timing_acceptance"])
            self.assertEqual(report["acceptance_status"], "unqualified")
            self.assertNotIn("passed", report)
            self.assertNotIn("PASS", completed.stdout)
            self.assertIn("UNQUALIFIED", completed.stdout)
            self.assertNotIn("criterion_median_nanoseconds", report)
            self.assertEqual(
                set(report["criterion_slope_nanoseconds_per_iteration"]),
                self.benchmarks,
            )

    def test_paired_ratios_use_median_but_remain_unqualified(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            heads = [root / f"head-{index}" for index in range(4)]
            bases = [root / f"base-{index}" for index in range(4)]
            for index, (head, base) in enumerate(zip(heads, bases)):
                head_slopes = {"analytical_external_q6/uqa": 4.0}
                base_slopes = {"analytical_external_q6/uqa": 4.0}
                if index == 2:
                    head_slopes["analytical_external_q1/uqa"] = 20.0
                self.write_criterion(head, head_slopes)
                self.write_criterion(base, base_slopes)

            output = root / "report.json"
            completed = self.run_checker(output, heads, bases)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("WARN external q6_uqa_vs_sqlite", completed.stdout)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertFalse(report["timing_acceptance"])
            self.assertEqual(report["acceptance_status"], "unqualified")
            self.assertFalse(report["external_limits_met"])
            self.assertTrue(report["regression_limits_met"])
            q6_external = next(
                gate
                for gate in report["external_ratio_checks"]
                if gate["name"] == "q6_uqa_vs_sqlite"
            )
            self.assertFalse(q6_external["within_reference_limit"])
            self.assertTrue(all(
                gate["within_reference_limit"] for gate in report["regression_ratio_checks"]
            ))
            q1_regression = next(
                gate
                for gate in report["regression_ratio_checks"]
                if gate["name"] == "q1_uqa_head_vs_base"
            )
            self.assertEqual(q1_regression["ratio"], 1.0)

    def test_repeated_slowdown_is_preserved_as_an_unqualified_observation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            heads = [root / f"head-{index}" for index in range(4)]
            bases = [root / f"base-{index}" for index in range(4)]
            for head, base in zip(heads, bases):
                self.write_criterion(head, {"analytical_external_q6/uqa": 1.2})
                self.write_criterion(base)

            output = root / "report.json"
            completed = self.run_checker(output, heads, bases)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("WARN regression q6_uqa_head_vs_base", completed.stdout)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertFalse(report["regression_limits_met"])
            self.assertFalse(report["timing_acceptance"])
            self.assertEqual(report["qualification_missing"], [
                "controlled_host_evidence", "independent_noise_bound",
            ])
            q6 = next(gate for gate in report["regression_ratio_checks"]
                      if gate["name"] == "q6_uqa_head_vs_base")
            self.assertEqual(q6["paired_ratios"], [1.2] * 4)
            self.assertEqual(q6["maximum"], 1.1)

    def test_non_finite_and_non_positive_samples_are_rejected(self) -> None:
        for value in [float("inf"), float("-inf"), float("nan"), 0.0, -1.0]:
            with self.subTest(value=value), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                criterion = root / "criterion"
                self.write_criterion(criterion, {"analytical_external_q1/uqa": value})
                output = root / "report.json"
                completed = self.run_checker(output, [criterion])
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertIn("finite and positive", completed.stderr)
                self.assertFalse(output.exists())

    def test_ratio_overflow_and_underflow_are_rejected(self) -> None:
        for numerator, denominator in [(1e308, 1e-308), (1e-308, 1e308)]:
            with self.subTest(numerator=numerator), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                criterion = root / "criterion"
                self.write_criterion(criterion, {
                    "analytical_external_q1/uqa": numerator,
                    "analytical_external_q1/sqlite": denominator,
                })
                output = root / "report.json"
                completed = self.run_checker(output, [criterion])
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertIn("ratio must be finite and positive", completed.stderr)
                self.assertFalse(output.exists())

    def test_overflowing_aggregate_is_rejected_before_writing_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            heads = [root / f"head-{index}" for index in range(4)]
            bases = [root / f"base-{index}" for index in range(4)]
            for criterion in heads + bases:
                self.write_criterion(criterion, {name: 1e308 for name in self.benchmarks})
            output = root / "report.json"
            completed = self.run_checker(output, heads, bases)
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("median must be finite", completed.stderr)
            self.assertFalse(output.exists())

    def test_paired_regression_rejects_a_different_workload(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            heads = [root / f"head-{index}" for index in range(4)]
            bases = [root / f"base-{index}" for index in range(4)]
            for head, base in zip(heads, bases):
                self.write_criterion(head)
                self.write_criterion(base)
            baseline_manifest = root / "baseline-manifest.json"
            baseline = dict(self.manifest)
            baseline["rows"] = int(baseline["rows"]) + 1
            baseline_manifest.write_text(json.dumps(baseline), encoding="utf-8")

            completed = self.run_checker(
                root / "report.json", heads, bases, baseline_manifest
            )

            self.assertEqual(completed.returncode, 2)
            self.assertIn("workload identities differ", completed.stderr)


if __name__ == "__main__":
    unittest.main()
