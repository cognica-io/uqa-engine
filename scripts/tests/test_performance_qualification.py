#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import datetime
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import performance_qualification as qualification


class SignedTimingFixture(unittest.TestCase):
    """Synthetic evidence only: never consume a historical machine report."""

    @classmethod
    def setUpClass(cls):
        cls.keys = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.keys.cleanup)
        cls.private_key = Path(cls.keys.name) / "private.pem"
        cls.public_key = Path(cls.keys.name) / "public.pem"
        subprocess.run(["openssl", "genrsa", "-out", str(cls.private_key), "2048"],
                       check=True, capture_output=True)
        subprocess.run(["openssl", "rsa", "-in", str(cls.private_key), "-pubout",
                        "-out", str(cls.public_key)], check=True, capture_output=True)

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.calibration_path = self.root / "calibration.json"
        self.run_path = self.root / "run.json"
        self.report = {
            "workload_identity_sha256": "a" * 64,
            "git_commit": "b" * 40,
            "benchmark_executable_sha256": "c" * 64,
            "criterion_slope_samples_nanoseconds_per_iteration": {"query": [100.0] * 4},
            "regression_protocol": {"pairs": 4, "ordering": "counterbalanced",
                                    "point_estimator": "median_of_paired_slope_ratios"},
            "baseline": {
                "workload_identity_sha256": "a" * 64,
                "git_commit": "d" * 40,
                "benchmark_executable_sha256": "e" * 64,
                "criterion_slope_samples_nanoseconds_per_iteration": {"query": [100.0] * 4},
            },
            "regression_ratio_checks": [{"benchmark": "query", "ratio": 1.0,
                                         "maximum": 1.1, "paired_ratios": [1.0] * 4}],
        }
        self.calibration = {
            "schema_version": 1, "kind": "independent_timing_calibration",
            "calibration_id": "independent-fixture", "host_id": "synthetic-host",
            "environment_id": "synthetic-environment",
            "completed_at": "2026-01-01T00:00:00Z",
            "valid_from": "2026-01-01T01:00:00Z",
            "valid_until": "2026-01-31T00:00:00Z",
            "independent_evidence": {"reference": "synthetic-test", "sha256": "f" * 64},
            "workload_identity_sha256": self.report["workload_identity_sha256"],
            "regression_protocol": copy.deepcopy(self.report["regression_protocol"]),
            "estimator": "median_of_paired_slope_ratios",
            "benchmarks": {"query": {"max_ratio": 1.1, "noise_factor": 1.02,
                                     "confidence_level": 0.99}},
        }
        self.run = {
            "schema_version": 1, "kind": "controlled_timing_run",
            "run_id": "candidate-fixture", "host_id": "synthetic-host",
            "environment_id": "synthetic-environment",
            "started_at": "2026-01-02T00:00:00Z",
            "finished_at": "2026-01-02T00:10:00Z",
            "exclusive_control": {"authority": "fixture-controller", "lease_id": "fixture-lease",
                                  "mechanism": "synthetic exclusive resources"},
        }

    def sign(self, path, contents=None):
        if contents is not None:
            path.write_bytes(contents)
        signature = path.with_suffix(".sig")
        subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.private_key),
                        "-out", str(signature), str(path)], check=True, capture_output=True)
        return signature

    def issue(self):
        self.sign(self.calibration_path, json.dumps(self.calibration).encode())
        self.run.setdefault("calibration_sha256", hashlib.sha256(self.calibration_path.read_bytes()).hexdigest())
        self.run.setdefault("observations_sha256", qualification.digest(qualification.observations(self.report)))
        self.sign(self.run_path, json.dumps(self.run).encode())

    def verify(self):
        return qualification.qualify(
            self.report, self.calibration_path, self.calibration_path.with_suffix(".sig"),
            self.run_path, self.run_path.with_suffix(".sig"), self.public_key,
            now=datetime.datetime(2026, 2, 1, tzinfo=datetime.timezone.utc),
        )


class PerformanceQualificationTest(SignedTimingFixture):
    def test_acceptance_requires_the_complete_noise_interval_to_meet_the_limit(self):
        for ratio, expected in [(1.0, "accepted"), (1.1, "inconclusive"), (1.2, "regression")]:
            with self.subTest(ratio=ratio):
                self.report["regression_ratio_checks"][0]["ratio"] = ratio
                self.run.pop("observations_sha256", None)
                self.issue()
                result = self.verify()
                self.assertEqual(result["acceptance_status"], expected)
                self.assertEqual(result["timing_acceptance"], expected == "accepted")
                self.assertEqual(result["qualification_missing"], [])
                low, high = result["qualified_ratio_checks"][0]["ratio_interval"]
                self.assertLessEqual(Fraction(low), Fraction(ratio) / Fraction(1.02))
                self.assertGreaterEqual(Fraction(high), Fraction(ratio) * Fraction(1.02))

    def test_rounding_at_the_limit_does_not_create_false_acceptance(self):
        limit = 1.1 * 1.1
        self.assertGreater(Fraction(1.1) ** 2, Fraction(limit))
        result = qualification.ratio_decision(1.1, limit, 1.1)
        self.assertEqual(result["decision"], "inconclusive")
        self.assertGreater(result["ratio_interval"][1], limit)
        self.assertEqual(qualification.ratio_decision(1.1, 1.1, 1.0)["decision"], "accepted")

    def test_invalid_arithmetic_is_rejected(self):
        for value in [True, None, "1.01", 0, -1, float("nan"), float("inf"), 10 ** 400]:
            with self.subTest(value=value), self.assertRaises(qualification.QualificationError):
                qualification.ratio_decision(value, 1.1, 1.02)
        for ratio, limit, noise in [(1, .9, 1), (1, 1.1, .9), (1e308, 1.1, 1e308),
                                    (1e-308, 1.1, 1e308)]:
            with self.subTest(ratio=ratio), self.assertRaises(qualification.QualificationError):
                qualification.ratio_decision(ratio, limit, noise)

    def test_signature_tampering_is_rejected_for_either_document(self):
        self.issue()
        for path in [self.calibration_path, self.run_path]:
            with self.subTest(path=path.name):
                original = path.read_bytes()
                path.write_bytes(original + b" ")
                with self.assertRaisesRegex(qualification.QualificationError, "signature"):
                    self.verify()
                path.write_bytes(original)

    def test_untrusted_key_and_truncated_signature_are_rejected(self):
        self.issue()
        signature = self.run_path.with_suffix(".sig")
        signature.write_bytes(signature.read_bytes()[:-1])
        with self.assertRaisesRegex(qualification.QualificationError, "signature"):
            self.verify()
        self.issue()
        original = self.public_key
        self.public_key = self.root / "untrusted.pem"
        self.public_key.write_bytes(b"invalid issuer key")
        with self.assertRaisesRegex(qualification.QualificationError, "signature"):
            self.verify()
        self.public_key = original

    def test_changed_samples_revisions_and_executables_cannot_reuse_an_attestation(self):
        self.issue()
        original = copy.deepcopy(self.report)
        mutations = [
            (self.report, "git_commit", "0" * 40),
            (self.report, "benchmark_executable_sha256", "0" * 64),
            (self.report["baseline"], "benchmark_executable_sha256", "0" * 64),
            (self.report["criterion_slope_samples_nanoseconds_per_iteration"], "query", [101.0] * 4),
        ]
        for target, key, value in mutations:
            previous = target[key]
            with self.subTest(key=key):
                target[key] = value
                with self.assertRaisesRegex(qualification.QualificationError, "observations"):
                    self.verify()
                target[key] = previous
        self.assertEqual(self.report, original)

    def test_signed_but_inapplicable_evidence_does_not_qualify(self):
        mutations = [
            ("run", "host_id", "other-host", "host_id differs"),
            ("run", "environment_id", "other-environment", "environment_id differs"),
            ("run", "run_id", "independent-fixture", "own noise"),
            ("run", "exclusive_control", None, "exclusive-control"),
            ("run", "exclusive_control", {"authority": "someone"}, "lease_id"),
            ("run", "calibration_sha256", "0" * 64, "different noise calibration"),
            ("run", "started_at", "2026-01-01T00:00:00Z", "does not cover"),
            ("run", "finished_at", "2026-02-02T00:00:00Z", "does not cover"),
            ("run", "started_at", "2026-01-02T00:00:00", "explicit UTC"),
            ("calibration", "independent_evidence", None, "independent calibration"),
            ("calibration", "workload_identity_sha256", "0" * 64, "different workload"),
            ("calibration", "regression_protocol", {"pairs": 2}, "sampling protocol"),
            ("calibration", "estimator", "mean", "estimator"),
            ("calibration", "benchmarks", {}, "inventory"),
            ("calibration", "schema_version", True, "unsupported"),
        ]
        original_calibration, original_run = copy.deepcopy(self.calibration), copy.deepcopy(self.run)
        for name, field, value, message in mutations:
            with self.subTest(name=name, field=field):
                self.calibration, self.run = copy.deepcopy(original_calibration), copy.deepcopy(original_run)
                getattr(self, name)[field] = value
                self.issue()
                with self.assertRaisesRegex(qualification.QualificationError, message):
                    self.verify()

    def test_unfinished_run_is_rejected_even_inside_calibration_validity(self):
        self.calibration["valid_until"] = "2026-03-01T00:00:00Z"
        self.run["finished_at"] = "2026-02-02T00:00:00Z"
        self.issue()
        with self.assertRaisesRegex(qualification.QualificationError, "unfinished"):
            self.verify()

    def test_changed_limits_and_absent_or_weak_noise_bounds_are_rejected(self):
        original = copy.deepcopy(self.calibration)
        for field, value in [("max_ratio", 1.2), ("noise_factor", None),
                             ("noise_factor", .9), ("confidence_level", .95),
                             ("confidence_level", 1.0)]:
            with self.subTest(field=field, value=value):
                self.calibration = copy.deepcopy(original)
                self.calibration["benchmarks"]["query"][field] = value
                self.run.pop("calibration_sha256", None)
                self.issue()
                with self.assertRaises(qualification.QualificationError):
                    self.verify()

    def test_signed_ambiguous_nonfinite_or_oversized_json_is_rejected(self):
        self.issue()
        for contents in [b'{"schema_version":1,"schema_version":1}',
                         b'{"schema_version":1,"value":NaN}', b'\xff', b'{' ,
                         b' ' * (1024 * 1024 + 1)]:
            with self.subTest(length=len(contents)):
                self.sign(self.calibration_path, contents)
                with self.assertRaises(qualification.QualificationError):
                    self.verify()


class AnalyticalQualificationCLITest(SignedTimingFixture):
    def setUp(self):
        super().setUp()
        self.manifest_path = SCRIPTS.parent / "benchmarks/analytical/manifest.json"
        self.manifest = json.loads(self.manifest_path.read_text())
        self.output = self.root / "report.json"
        self.executable = self.root / "synthetic-benchmark"
        self.executable.write_bytes(b"synthetic executable fixture; not a measured program")
        self.command = [sys.executable, str(SCRIPTS / "check-analytical-benchmark.py"),
                        "--output", str(self.output), "--baseline-manifest", str(self.manifest_path),
                        "--baseline-revision", "d" * 40, "--head-executable", str(self.executable),
                        "--baseline-executable", str(self.executable)]
        benchmarks = {name for gate in self.manifest["external_ratio_checks"]
                      for name in (gate["numerator"], gate["denominator"])}
        benchmarks.update(gate["benchmark"] for gate in self.manifest["regression_gates"])
        self.head_estimates = []
        for role in ["head", "baseline"]:
            for pair in range(self.manifest["regression_protocol"]["pairs"]):
                root = self.root / f"{role}-{pair}"
                flag = "--criterion-root" if role == "head" else "--baseline-criterion-root"
                self.command.extend([flag, str(root)])
                for benchmark in benchmarks:
                    path = root / benchmark / "new/estimates.json"
                    path.parent.mkdir(parents=True)
                    path.write_text(json.dumps({"slope": {"point_estimate": 1.0}}))
                    if role == "head":
                        self.head_estimates.append(path)

    def execute(self, evidence=False, extra=None):
        args = list(self.command)
        if evidence:
            args.extend(["--require-qualified", "--calibration", str(self.calibration_path),
                         "--calibration-signature", str(self.calibration_path.with_suffix(".sig")),
                         "--run-attestation", str(self.run_path),
                         "--run-signature", str(self.run_path.with_suffix(".sig")),
                         "--issuer-key", str(self.public_key)])
        return subprocess.run(args + (extra or []), text=True, capture_output=True)

    def issue_checker_evidence(self):
        result = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.report = json.loads(self.output.read_text())
        self.calibration["workload_identity_sha256"] = self.report["workload_identity_sha256"]
        self.calibration["regression_protocol"] = self.report["regression_protocol"]
        self.calibration["benchmarks"] = {
            gate["benchmark"]: {"max_ratio": gate["maximum"], "noise_factor": 1.02,
                                "confidence_level": 0.99}
            for gate in self.report["regression_ratio_checks"]
        }
        self.run.pop("calibration_sha256", None)
        self.run.pop("observations_sha256", None)
        self.issue()

    def test_cli_exit_status_distinguishes_accepted_inconclusive_and_regression(self):
        for ratio, status, exit_code in [(1.0, "accepted", 0), (1.1, "inconclusive", 3),
                                        (1.2, "regression", 1)]:
            with self.subTest(ratio=ratio):
                for path in self.head_estimates:
                    path.write_text(json.dumps({"slope": {"point_estimate": ratio}}))
                self.issue_checker_evidence()
                result = self.execute(evidence=True)
                self.assertEqual(result.returncode, exit_code, result.stderr)
                report = json.loads(self.output.read_text())
                self.assertEqual(report["acceptance_status"], status)
                self.assertEqual(report["timing_acceptance"], exit_code == 0)
                self.assertIn(status.upper(), result.stdout)

    def test_invalid_evidence_replaces_a_previous_accepted_report(self):
        self.issue_checker_evidence()
        result = self.execute(evidence=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.run_path.write_bytes(self.run_path.read_bytes() + b" ")
        result = self.execute(evidence=True)
        self.assertEqual(result.returncode, 2, result.stderr)
        report = json.loads(self.output.read_text())
        self.assertFalse(report["timing_acceptance"])
        self.assertEqual(report["acceptance_status"], "invalid")
        self.assertNotIn("qualification", report)
        self.assertIn("signature", report["qualification_error"])

    def test_required_or_partial_evidence_cannot_fall_back_to_diagnostics(self):
        for extra in [["--require-qualified"], ["--calibration", str(self.calibration_path)]]:
            with self.subTest(extra=extra):
                result = self.execute(extra=extra)
                self.assertEqual(result.returncode, 2)
                self.assertIn("qualified timing requires", result.stderr)
                self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
