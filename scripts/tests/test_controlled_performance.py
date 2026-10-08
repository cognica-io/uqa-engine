#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from controlled_performance import analytical_observations, claim_observations, scaling_summary
from performance_qualification import QualificationError


class ControlledWorkloadTest(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).resolve().parents[2] / "benchmarks/regressions/claim-timing.json"
        self.manifest = json.loads(path.read_text())
        self.claims = {"schema_version": 1, "measurement": True, "cases": [
            {"processes": p, "keys_per_process": keys, "warmup_rounds": 8, "rounds": 128,
             "correctness": True, "samples": [{"acquire_ns": [10] * p, "release_ns": [5] * p,
             "acquisition_wall_ns": p * 20, "release_wall_ns": p * 10} for _ in range(128)]}
            for p in [1, 2, 4, 8] for keys in [64, 512]]}

    def test_throughput_uses_complete_phase_windows(self):
        summary = claim_observations(self.claims, self.manifest)
        self.assertEqual(len(summary), 16)
        rows = scaling_summary({name: [value] * 4 for name, value in summary.items()}, self.manifest)
        self.assertTrue(all(row["throughput_relative_to_one_process"] == 1 for row in rows))
        self.assertEqual(summary["claims/8/512/acquire"], 160)

    def test_missing_duplicate_and_incomplete_claim_cases_fail(self):
        missing, duplicate, incomplete, failed, bad_window = [copy.deepcopy(self.claims) for _ in range(5)]
        missing["cases"].pop()
        duplicate["cases"][-1] = duplicate["cases"][0]
        incomplete["cases"][0]["samples"].pop()
        failed["cases"][0]["correctness"] = False
        bad_window["cases"][0]["samples"][0]["acquisition_wall_ns"] = 1
        for report in [missing, duplicate, incomplete, failed, bad_window]:
            with self.subTest(report=report["cases"][0]["correctness"]), self.assertRaises(QualificationError):
                claim_observations(report, self.manifest)

    def test_analytical_reader_checks_actual_sample_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / "query/uqa/new"
            directory.mkdir(parents=True)
            (directory / "estimates.json").write_text(json.dumps({"slope": {"point_estimate": 50}}))
            sample = {"sampling_mode": "Linear", "iters": list(range(1, 21)), "times": [100] * 20}
            (directory / "sample.json").write_text(json.dumps(sample))
            self.assertEqual(analytical_observations(root, ["query/uqa"], 20), {"query/uqa": 50})
            sample["times"].pop()
            (directory / "sample.json").write_text(json.dumps(sample))
            with self.assertRaises(QualificationError):
                analytical_observations(root, ["query/uqa"], 20)


if __name__ == "__main__":
    unittest.main()
