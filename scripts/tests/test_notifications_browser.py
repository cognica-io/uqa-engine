#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Browser notification acceptance must not accept a partial or failed run."""

import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify-notifications-browser.py"
SPEC = importlib.util.spec_from_file_location("notification_browser", SCRIPT)
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)


class NotificationBrowserReportTests(unittest.TestCase):
    @staticmethod
    def complete():
        return {"browserVersion": "150.0.0.0", "report": {
            "schema_version": 1, "status": "Passed", "error": None,
            "passed": list(VERIFIER.EXPECTED),
        }}

    def test_complete_actual_case_inventory(self):
        VERIFIER.verify_report(self.complete())

    def test_failures_and_partial_inventories_are_rejected(self):
        for change in ({"schema_version": 2}, {"schema_version": True}, {"status": "Running"}, {"status": "Failed"},
                       {"error": "network failed"}, {"passed": []},
                       {"passed": VERIFIER.EXPECTED[:-1]},
                       {"passed": [VERIFIER.EXPECTED[0]] * len(VERIFIER.EXPECTED)},
                       {"passed": VERIFIER.EXPECTED + ["unverified"]}):
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                value = self.complete()
                value["report"].update(change)
                VERIFIER.verify_report(value)
        for value in (None, {}, {"browserVersion": ""}, {"browserVersion": "150", "report": None}):
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                VERIFIER.verify_report(value)


if __name__ == "__main__":
    unittest.main()
