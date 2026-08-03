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
    def test_gate_uses_linear_slope_instead_of_sample_median(self) -> None:
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        benchmarks = {
            name
            for gate in manifest["ratio_gates"]
            for name in (gate["numerator"], gate["denominator"])
        }

        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            criterion = root / "criterion"
            for benchmark in benchmarks:
                estimates = criterion.joinpath(*benchmark.split("/"), "new", "estimates.json")
                estimates.parent.mkdir(parents=True)
                median = 100.0 if benchmark.endswith("/uqa") else 1.0
                estimates.write_text(
                    json.dumps(
                        {
                            "median": {"point_estimate": median},
                            "slope": {"point_estimate": 1.0},
                        }
                    ),
                    encoding="utf-8",
                )

            output = root / "report.json"
            completed = subprocess.run(
                [
                    "python3",
                    str(CHECKER),
                    "--criterion-root",
                    str(criterion),
                    "--output",
                    str(output),
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], 2)
            self.assertNotIn("criterion_median_nanoseconds", report)
            self.assertEqual(
                set(report["criterion_slope_nanoseconds_per_iteration"]), benchmarks
            )


if __name__ == "__main__":
    unittest.main()
