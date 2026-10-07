#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import importlib.util
import json
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("performance_checks", ROOT / "scripts/run-performance-checks.py")
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class PerformanceChecksTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        patch = mock.patch.object(runner, "ROOT", self.root)
        patch.start()
        self.addCleanup(patch.stop)
        self.check = {
            "package": "uqa-example", "kind": "lib", "test": "bounded_work", "cases": 2,
            "source": "crates/uqa-example/src/tests.rs", "capability": "lookup",
            "workload": "two provider cases", "invariant": "no unrelated row reads",
        }
        source = self.root / self.check["source"]
        source.parent.mkdir(parents=True)
        source.write_text("fn bounded_work() {}\n", encoding="utf-8")
        self.manifest = self.root / "manifest.json"
        self.suite = {
            "package-name": "uqa-example", "kind": "lib", "status": "listed",
            "testcases": {
                "tests::bounded_work::memory": {"ignored": False, "filter-match": {"status": "matches"}},
                "tests::bounded_work::sqlite": {"ignored": False, "filter-match": {"status": "matches"}},
                "tests::unrelated": {"ignored": False, "filter-match": {"status": "mismatch"}},
            },
        }
        self.listing = {"rust-suites": {"uqa-example": self.suite}}

    def write_inventory(self, checks=None, schema=1):
        self.manifest.write_text(json.dumps({"schema_version": schema, "checks": checks if checks is not None else [self.check]}))
        return runner.inventory(self.manifest)

    def test_current_repository_inventory_is_valid(self):
        with mock.patch.object(runner, "ROOT", ROOT):
            checks = runner.inventory()
        self.assertGreater(len(checks), 0)

    def test_source_inventory_rejects_missing_and_changed_test_names(self):
        self.check["test"] = "missing"
        with self.assertRaisesRegex(ValueError, "missing test definition"):
            self.write_inventory()

    def test_source_cannot_escape_or_claim_another_owner(self):
        for source in ("/etc/passwd", "../outside.rs", "crates/uqa-other/src/tests.rs"):
            with self.subTest(source=source):
                self.check["source"] = source
                with self.assertRaisesRegex(ValueError, "source must belong"):
                    self.write_inventory()

    def test_duplicate_empty_and_unknown_inventories_fail(self):
        for checks, schema in (([self.check, self.check], 1), ([], 1), ([self.check], 2)):
            with self.subTest(checks=checks, schema=schema), self.assertRaises(ValueError):
                self.write_inventory(checks, schema)

    def test_counts_must_be_positive_integers(self):
        for count in (0, -1, True, 1.5):
            self.check["cases"] = count
            with self.subTest(count=count), self.assertRaisesRegex(ValueError, "case count"):
                self.write_inventory()

    def test_filter_syntax_cannot_be_injected(self):
        for field, value in (("package", "uqa-example) | all("), ("test", "work) | all("), ("kind", "bench")):
            changed = {**self.check, field: value}
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.write_inventory([changed])

    def test_selection_counts_all_provider_cases_and_records_source(self):
        result = runner.verify_selection([self.check], self.listing)
        self.assertEqual(result[0]["selected_cases"], 2)
        self.assertEqual(len(result[0]["source_sha256"]), 64)

    def test_zero_or_missing_provider_cases_fail(self):
        for removed in (("tests::bounded_work::sqlite",), ("tests::bounded_work::sqlite", "tests::bounded_work::memory")):
            listing = copy.deepcopy(self.listing)
            for name in removed:
                del listing["rust-suites"]["uqa-example"]["testcases"][name]
            with self.subTest(removed=removed), self.assertRaisesRegex(ValueError, "expected 2 cases"):
                runner.verify_selection([self.check], listing)

    def test_ignored_and_filtered_required_cases_fail(self):
        for change in ({"ignored": True}, {"filter-match": {"status": "mismatch"}}):
            listing = copy.deepcopy(self.listing)
            listing["rust-suites"]["uqa-example"]["testcases"]["tests::bounded_work::memory"].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "ignored or filtered"):
                runner.verify_selection([self.check], listing)

    def test_wrong_package_or_target_cannot_satisfy_coverage(self):
        for change in ({"package-name": "uqa-other"}, {"kind": "test"}):
            listing = copy.deepcopy(self.listing)
            listing["rust-suites"]["uqa-example"].update(change)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "found 0"):
                runner.verify_selection([self.check], listing)

    def test_unreviewed_and_overlapping_selections_fail(self):
        with self.assertRaisesRegex(ValueError, "overlapping"):
            runner.verify_selection([self.check, self.check], self.listing)
        self.suite["testcases"]["tests::unrelated"]["filter-match"]["status"] = "matches"
        with self.assertRaisesRegex(ValueError, "outside the reviewed inventory"):
            runner.verify_selection([self.check], self.listing)

    def test_similar_name_does_not_replace_a_required_test(self):
        self.suite["testcases"]["tests::bounded_work_extra"] = self.suite["testcases"].pop("tests::bounded_work::sqlite")
        with self.assertRaisesRegex(ValueError, "found 1"):
            runner.verify_selection([self.check], self.listing)

    def test_arguments_select_only_existing_test_targets(self):
        args = runner.arguments([self.check, {**self.check, "test": "other"}])
        self.assertEqual(args.count("-p"), 1)
        self.assertIn("--locked", args)
        self.assertIn("--lib", args)
        self.assertIn("--tests", args)
        self.assertNotIn("--all-targets", args)
        self.assertIn("test(/(^|::)bounded_work(::.*)?$/)", args[-1])

    def test_failure_report_retains_diagnostic_and_does_not_claim_timing_acceptance(self):
        output = self.root / "output"
        result = {"status": "failed", "revision": "a" * 40, "checks": [], "timing_acceptance": False, "error": "missing provider case"}
        with mock.patch.dict("os.environ", {"GITHUB_STEP_SUMMARY": str(self.root / "step.md")}):
            runner.report(output, result)
        self.assertEqual(json.loads((output / "summary.json").read_text()), result)
        self.assertIn("**failed**", (output / "summary.md").read_text())
        self.assertIn("missing provider case", (self.root / "step.md").read_text())


if __name__ == "__main__":
    unittest.main()
