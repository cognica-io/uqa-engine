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


class AnalyticalBenchmarkGateTest(unittest.TestCase):
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

    def test_gate_uses_linear_slope_instead_of_sample_median(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            criterion = root / "criterion"
            self.write_criterion(criterion, misleading_median=True)

            output = root / "report.json"
            completed = self.run_checker(output, [criterion])

            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], 3)
            self.assertNotIn("criterion_median_nanoseconds", report)
            self.assertEqual(
                set(report["criterion_slope_nanoseconds_per_iteration"]),
                self.benchmarks,
            )

    def test_paired_regression_gate_uses_median_and_makes_external_ratios_advisory(
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
            self.assertFalse(report["external_ratios_enforced"])
            q6_external = next(
                gate
                for gate in report["external_ratio_checks"]
                if gate["name"] == "q6_uqa_vs_sqlite"
            )
            self.assertFalse(q6_external["passed"])
            self.assertTrue(all(gate["passed"] for gate in report["regression_gates"]))
            q1_regression = next(
                gate
                for gate in report["regression_gates"]
                if gate["name"] == "q1_uqa_head_vs_base"
            )
            self.assertEqual(q1_regression["ratio"], 1.0)

    def test_paired_regression_gate_fails_a_repeatable_slowdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            heads = [root / f"head-{index}" for index in range(4)]
            bases = [root / f"base-{index}" for index in range(4)]
            for head, base in zip(heads, bases):
                self.write_criterion(head, {"analytical_external_q6/uqa": 1.2})
                self.write_criterion(base)

            output = root / "report.json"
            completed = self.run_checker(output, heads, bases)

            self.assertEqual(completed.returncode, 1, completed.stderr)
            self.assertIn("FAIL regression q6_uqa_head_vs_base", completed.stdout)

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
