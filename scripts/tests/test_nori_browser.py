#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_browser", ROOT / "scripts/verify-nori-browser.py")
browser = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(browser)
EVIDENCE = ROOT / "benchmarks/nori/browser-evidence"


class NoriBrowserTest(unittest.TestCase):
    def setUp(self):
        self.report = json.loads((EVIDENCE / "chrome-macos-enabled.json").read_text())["runs"][0]

    def test_recorded_browser_evidence_is_complete_and_hash_pinned(self):
        manifest = json.loads((EVIDENCE / "manifest.json").read_text())
        self.assertEqual(len(manifest["reports"]), 2)
        for item in manifest["reports"]:
            path = EVIDENCE / item["file"]
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), item["sha256"])
            report = json.loads(path.read_text())
            self.assertEqual(len(report["runs"]), 3)
            for run in report["runs"]:
                browser.verify_run(run, report["feature"])

    def test_omitted_steps_and_fake_reloads_fail(self):
        for mutation in [lambda r: r["completed_steps"].pop(), lambda r: r["pages"].__setitem__(1, r["pages"][0]), lambda r: r["checkpoints"].pop()]:
            report = copy.deepcopy(self.report)
            mutation(report)
            with self.assertRaises(RuntimeError):
                browser.verify_run(report, "enabled")

    def test_missing_indexeddb_bytes_fail(self):
        self.report["checkpoints"][0]["storage"]["usageDetails"]["indexedDB"] = 0
        with self.assertRaisesRegex(RuntimeError, "IndexedDB"):
            browser.verify_run(self.report, "enabled")

    def test_changed_complete_diagnostics_fail(self):
        self.report["analyses"][-1]["diagnostic_sha256"] = "0" * 64
        with self.assertRaisesRegex(RuntimeError, "diagnostics"):
            browser.verify_run(self.report, "enabled")

    def test_missing_or_invalid_memory_observations_fail(self):
        for mutation in [lambda r: r["memory"].pop(), lambda r: r["memory"][0].__setitem__("bytes", -1)]:
            report = copy.deepcopy(self.report)
            mutation(report)
            with self.assertRaisesRegex(RuntimeError, "memory"):
                browser.verify_run(report, "enabled")

    def test_requested_feature_must_match(self):
        with self.assertRaisesRegex(RuntimeError, "feature"):
            browser.verify_run(self.report, "disabled")

    def test_cli_errors_cannot_be_read_as_results(self):
        with self.assertRaisesRegex(RuntimeError, "no result"):
            browser.result_from_cli("### Error\nThe browser failed")


if __name__ == "__main__":
    unittest.main()
