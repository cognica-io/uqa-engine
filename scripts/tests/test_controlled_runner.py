#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

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
    def test_runtime_sources_remain_exact_when_claim_reference_build_reuses_candidate_checkout(self):
        repository = Path(__file__).resolve().parents[2]
        analytical = json.loads((repository / "benchmarks/analytical/manifest.json").read_text())
        claims = json.loads((repository / "benchmarks/regressions/claim-timing.json").read_text())
        analytical_identity = controller.analytical_identity(analytical)
        for cached in (False, True):
            with self.subTest(cached_claim_reference=cached), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                installed, reference, candidate = (root / name for name in ("controller", "reference", "candidate"))
                installed.mkdir()
                (installed / "analytical-manifest.json").write_text(json.dumps(analytical))
                (installed / "claim-timing.json").write_text(json.dumps(claims))
                (installed / "adapter.py").write_text("# installed controller\n")
                (candidate / "scripts").mkdir(parents=True)
                (candidate / "scripts/adapter.py").write_text((installed / "adapter.py").read_text())
                for path in (reference, candidate):
                    (path / "benchmarks/analytical").mkdir(parents=True)
                    (path / "benchmarks/analytical/manifest.json").write_text(json.dumps(analytical))
                (candidate / "benchmarks/regressions").mkdir()
                (candidate / "benchmarks/regressions/claim-timing.json").write_text(json.dumps(claims))
                head = "a" * 40
                shared_revision = None

                def source(revision, role):
                    nonlocal shared_revision
                    if role == "reference":
                        self.assertEqual(revision, controller.REFERENCE)
                        return reference
                    shared_revision = revision
                    return candidate

                def build(revision, role, include_claims):
                    if role == "claims-reference" and not cached:
                        source(revision, role)
                    return {name: {"revision": revision} for name in ("analytical_comparison", "row_claim_contention")}

                def compare(host, kind, current, baseline, *args):
                    self.assertEqual(shared_revision, head)
                    self.assertEqual(current[0], candidate if kind == "analytical" else None)
                    self.assertEqual(baseline[0], reference if kind == "analytical" else None)
                    self.assertEqual(current[1]["revision"], head)
                    self.assertEqual(baseline[1]["revision"], controller.REFERENCE if kind == "analytical"
                                     else claims["reference_revision"])
                    (root / (kind + "-report.json")).write_text("{}")
                    return {"acceptance_status": "accepted"}

                host = Mock(source=Mock(side_effect=source), build=Mock(side_effect=build),
                            isolate=Mock(return_value={}), output=root, run_id="test", config={"instance_id": "test"})
                with patch.object(controller, "HERE", installed), patch.object(controller, "calibrate", return_value={}), \
                        patch.object(controller, "analytical_identity", return_value=analytical_identity), \
                        patch.object(controller, "compare", side_effect=compare):
                    result = controller.execute(host, head)
                self.assertEqual(result["head_revision"], head)
                self.assertEqual(set(result["reports"]), {"analytical", "claims"})

    def test_instance_is_stopped_when_execution_or_artifact_collection_fails(self):
        for upload_failed in [False, True]:
            calls = []

            def aws(*args):
                calls.append(args)
                if args[:2] == ("ssm", "send-command"):
                    raise RuntimeError("SSM unavailable")

            environment = {"PERFORMANCE_INSTANCE_ID": "instance", "PERFORMANCE_ARTIFACT_BUCKET": "bucket",
                           "GITHUB_SHA": "a" * 40, "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1"}
            with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, environment), \
                    patch.object(sys, "argv", ["run-ec2-performance.py", "--output", temporary]), \
                    patch.object(ci, "aws", side_effect=aws), patch.object(ci, "await_online"), \
                    patch.object(ci.subprocess, "run", side_effect=RuntimeError("S3 unavailable") if upload_failed else None), \
                    self.assertRaises(RuntimeError):
                ci.main()
            self.assertIn(("ec2", "stop-instances", "--instance-ids", "instance"), calls)
            self.assertIn(("ec2", "wait", "instance-stopped", "--instance-ids", "instance"), calls)

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
