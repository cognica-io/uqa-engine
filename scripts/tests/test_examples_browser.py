#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reject incomplete or same-page browser example acceptance evidence."""

import importlib.util
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify-examples-browser.py"
SPEC = importlib.util.spec_from_file_location("verify_examples_browser", SCRIPT)
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)


class BrowserExampleReportTests(unittest.TestCase):
    def test_complete_checkpoint_and_fresh_restore(self):
        VERIFIER.verify_report(self.complete_report())

    def test_partial_or_invalid_evidence_is_rejected(self):
        for changed in (
            {"schema_version": 2},
            {"error": "reopen failed"},
            {"status": "Awaiting page reload"},
            {"completed_examples": ["vector-knn"]},
            {"completed_examples": ["vector-knn"] * 5},
            {"restored_diskann": False},
            {"pages": ["first"]},
            {"pages": ["first", "first"]},
            {"pages": ["first", "second", "third"]},
            {"checkpoint_databases": []},
        ):
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                report = self.complete_report()
                report.update(changed)
                VERIFIER.verify_report(report)

    @staticmethod
    def complete_report():
        return {
            "schema_version": 1,
            "status": "Passed: five examples and fresh-page DiskANN restore",
            "completed_examples": [
                "unified-search", "vector-knn", "graph-cypher",
                "storage-transactions", "extensibility",
            ],
            "pages": ["first", "second"],
            "checkpoint_databases": ["/uqa"],
            "restored_diskann": True,
        }


if __name__ == "__main__":
    unittest.main()
