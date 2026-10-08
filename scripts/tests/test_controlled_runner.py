#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from performance_qualification import QualificationError


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).resolve().parents[1] / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


controller = load("run-controlled-performance")
ci = load("run-ec2-performance")


class ControlledRunnerTest(unittest.TestCase):
    def test_all_pairs_are_retained_in_counterbalanced_role_order(self):
        class Host:
            def __init__(self, output):
                self.output, self.calls = output, []

            def progress(self, *args, **kwargs):
                pass

            def measure(self, kind, artifact, source, label, manifest):
                self.calls.append((artifact, label))
                return {"query": len(self.calls)}

        with tempfile.TemporaryDirectory() as temporary:
            host = Host(Path(temporary))
            pairs = controller.paired_samples(host, "analytical", (None, "head"), (None, "base"), {}, 4, "comparison")
            self.assertEqual([call[0] for call in host.calls], ["head", "base", "base", "head"] * 2)
            self.assertEqual([pair["first"]["query"] for pair in pairs], [1, 4, 5, 8])
            self.assertEqual([pair["second"]["query"] for pair in pairs], [2, 3, 6, 7])
            self.assertTrue((host.output / "analytical-comparison-observations.json").is_file())

    def test_ci_rejects_incomplete_uncertain_or_mismatched_signed_results(self):
        valid = {
            "result.json": ({"head_revision": "a" * 40, "run_id": "123-1", "reports": {
                name: {"sha256": name, "acceptance_status": "accepted"} for name in ["analytical", "claims"]}}, "result"),
            **{name + "-report.json": ({"git_commit": "a" * 40, "acceptance_status": "accepted", "timing_acceptance": True}, name)
               for name in ["analytical", "claims"]}}
        variants = []
        for status in ["inconclusive", "regression", "invalid"]:
            changed = copy.deepcopy(valid)
            changed["claims-report.json"][0]["acceptance_status"] = status
            changed["result.json"][0]["reports"]["claims"]["acceptance_status"] = status
            variants.append(changed)
        changed = copy.deepcopy(valid)
        del changed["result.json"][0]["reports"]["claims"]
        variants.append(changed)
        changed = copy.deepcopy(valid)
        changed["analytical-report.json"][0]["git_commit"] = "b" * 40
        variants.append(changed)
        changed = copy.deepcopy(valid)
        changed["result.json"][0]["reports"]["analytical"]["sha256"] = "replaced"
        variants.append(changed)
        with patch.object(ci, "verified_document", side_effect=lambda path, *args: valid[path.name]):
            ci.verify_result(Path("evidence"), b"operator key", "a" * 40, "123-1")
        for variant in variants:
            with patch.object(ci, "verified_document", side_effect=lambda path, *args: variant[path.name]), self.assertRaises(QualificationError):
                ci.verify_result(Path("evidence"), b"operator key", "a" * 40, "123-1")


if __name__ == "__main__":
    unittest.main()
