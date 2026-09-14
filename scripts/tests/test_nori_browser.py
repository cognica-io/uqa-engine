#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_browser", ROOT / "scripts/verify-morphology-browser.py")
browser = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(browser)
FIXTURE = ROOT / "scripts/tests/fixtures/nori_browser.json"


class NoriBrowserTest(unittest.TestCase):
    def fixture(self, feature):
        report = json.loads(FIXTURE.read_text())[feature]
        contract = json.loads((ROOT / "benchmarks/nori/browser-contract.json").read_text())
        report["analyses"] = contract["checks"] if feature == "enabled" else []
        return report

    def setUp(self):
        self.report = self.fixture("enabled")

    def test_complete_enabled_and_disabled_reports_pass(self):
        for feature in ("enabled", "disabled"):
            with self.subTest(feature=feature):
                browser.verify_run(self.fixture(feature), feature)

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

    def test_functional_reports_validate_both_languages_without_memory_sampling(self):
        for language in ("nori", "kuromoji"):
            fixture = json.loads((ROOT / f"tests/parity/{language}/bindings.json").read_text())
            for feature in ("enabled", "disabled"):
                report = self.fixture(feature)
                steps = fixture[feature]
                report.update(language=language, observe_memory=False, memory=[],
                              completed_steps=[step["name"] for step in steps])
                if language == "kuromoji":
                    report["analyses"] = []
                checkpoints = [index + 1 for index, step in enumerate(steps) if step.get("reopen")]
                sample = report["checkpoints"][0]
                report["checkpoints"] = [dict(sample, next_step=index) for index in checkpoints]
                report["pages"] = [f"page-{index}" for index in range(len(checkpoints) + 1)]
                report["status"] = f"Passed: {len(steps)} steps across {len(report['pages'])} page loads"
                with self.subTest(language=language, feature=feature):
                    browser.verify_run(report, feature, language, False)
                    for mutation in (lambda r: r["memory"].append({"bytes": 1}),
                                     lambda r: r["completed_steps"].pop(),
                                     lambda r: r["checkpoints"].pop(),
                                     lambda r: r.update(language="wrong")):
                        changed = copy.deepcopy(report)
                        mutation(changed)
                        with self.assertRaises(RuntimeError):
                            browser.verify_run(changed, feature, language, False)

    def test_cli_errors_cannot_be_read_as_results(self):
        with self.assertRaisesRegex(RuntimeError, "no result"):
            browser.result_from_cli("### Error\nThe browser failed")


if __name__ == "__main__":
    unittest.main()
