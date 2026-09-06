#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import argparse
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
HERE = ROOT / "tests/parity/pg18/upstream"
SPEC = importlib.util.spec_from_file_location("pg18_upstream_harness", HERE / "harness.py")
assert SPEC is not None and SPEC.loader is not None
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class UpstreamInventoryTest(unittest.TestCase):
    def setUp(self) -> None:
        self.inventory = HARNESS.read_json(HERE / "inventory.json")
        self.coverage = HARNESS.read_json(HERE / "coverage.json")

    def test_pinned_corpus_includes_both_schedules_all_extras_and_license(self) -> None:
        HARNESS.validate_inventory(self.inventory, self.coverage)
        self.assertEqual(len(self.inventory["files"]), 791)
        self.assertEqual(HARNESS.digest(HERE / "COPYRIGHT"), self.inventory["files"]["COPYRIGHT"]["sha256"])
        for suite, scheduled, total, extras in (
            ("core", 231, 233, {"numeric_big", "reindex_catalog"}),
            ("isolation", 119, 121, {"prepared-transactions", "prepared-transactions-cic"}),
        ):
            data = self.inventory["suites"][suite]
            names = [name for group in data["groups"] for name in group]
            self.assertEqual(len(names), scheduled)
            self.assertEqual(len(data["tests"]), total)
            self.assertEqual(set(data["tests"]) - set(names), extras)

    def test_platform_variants_remain_in_the_inventory(self) -> None:
        outputs = self.inventory["suites"]["core"]["tests"]["float4"]["expected"]
        self.assertIn("src/test/regress/expected/float4.out", outputs)
        self.assertIn("src/test/regress/expected/float4-misrounded-input.out", outputs)

    def test_no_test_can_be_omitted_from_or_added_to_coverage(self) -> None:
        del self.coverage["tests"]["core/numeric_big"]
        with self.assertRaisesRegex(ValueError, "every scheduled and extra test"):
            HARNESS.validate_inventory(self.inventory, self.coverage)
        self.coverage = HARNESS.read_json(HERE / "coverage.json")
        self.coverage["tests"]["core/invented"] = copy.deepcopy(self.coverage["tests"]["core/numeric_big"])
        with self.assertRaisesRegex(ValueError, "every scheduled and extra test"):
            HARNESS.validate_inventory(self.inventory, self.coverage)

    def test_schedule_positions_and_expected_files_are_checked(self) -> None:
        self.inventory["suites"]["core"]["tests"]["numeric_big"]["schedule_group"] = 1
        with self.assertRaisesRegex(ValueError, "schedule position"):
            HARNESS.validate_inventory(self.inventory, self.coverage)
        self.inventory = HARNESS.read_json(HERE / "inventory.json")
        del self.inventory["files"]["src/test/regress/expected/numeric_big.out"]
        with self.assertRaisesRegex(ValueError, "missing input or output"):
            HARNESS.validate_inventory(self.inventory, self.coverage)

    def test_reference_only_evidence_cannot_verify_compatibility(self) -> None:
        case = self.coverage["tests"]["core/numeric_big"]
        case.update(status="verified", evidence={"postgres": "reference/report.json"})
        with self.assertRaisesRegex(ValueError, "PostgreSQL and UQA evidence"):
            HARNESS.validate_inventory(self.inventory, self.coverage)
        case["evidence"]["uqa"] = None
        case["evidence"]["postgres"] = None
        with self.assertRaisesRegex(ValueError, "missing report path"):
            HARNESS.validate_inventory(self.inventory, self.coverage)

    def test_each_batch_preserves_the_upstream_order_and_no_extra_is_dropped(self) -> None:
        batches = HARNESS.batches(self.inventory, "all")
        ids = [f"{batch['suite']}/{test}" for batch in batches for test in batch["tests"]]
        self.assertEqual(len(ids), 354)
        self.assertEqual(len(set(ids)), 354)
        self.assertEqual(set(ids), set(self.coverage["tests"]))
        self.assertEqual(batches[1]["tests"], ["reindex_catalog"])
        self.assertIsNone(batches[1]["schedule"])
        self.assertEqual(batches[0]["tests"][:-1], [name for group in self.inventory["suites"]["core"]["groups"] for name in group])
        self.assertEqual(batches[2]["extras"], ["prepared-transactions", "prepared-transactions-cic"])


class UpstreamScheduleTest(unittest.TestCase):
    def test_parallel_groups_and_platform_names_are_preserved(self) -> None:
        self.assertEqual(HARNESS.parse_schedule("# dependency\ntest: setup\n\ntest: float4 collate.icu.utf8\n"), [["setup"], ["float4", "collate.icu.utf8"]])

    def test_skip_directives_duplicates_and_traversal_are_rejected(self) -> None:
        for text in ("ignore: broken\n", "test: a\ntest: a\n", "test: a a\n", "test: ../escape\n", "test: \n", "# empty\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                HARNESS.parse_schedule(text)


class UpstreamArchiveTest(unittest.TestCase):
    def make_archive(self, parent: Path, members: list[tuple[str, str]]) -> tuple[Path, dict]:
        archive = parent / "test.tar.bz2"
        with tarfile.open(archive, "w:bz2") as target:
            for name, kind in members:
                info = tarfile.TarInfo(name)
                if kind == "symlink":
                    info.type = tarfile.SYMTYPE
                    info.linkname = "../../outside"
                    target.addfile(info)
                else:
                    info.size = 3
                    target.addfile(info, io.BytesIO(b"sql"))
        return archive, {"root": "postgresql-18.4", "bytes": archive.stat().st_size, "sha256": HARNESS.digest(archive)}

    def test_license_inputs_expected_and_support_files_are_imported_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            names = ["COPYRIGHT", "src/test/regress/sql/a.sql", "src/test/regress/expected/a.out", "src/test/regress/data/fixture.data", "src/test/isolation/specs/locks.spec", "src/test/isolation/expected/locks.out", "src/backend/not-imported.c"]
            archive, pin = self.make_archive(parent, [("postgresql-18.4/" + name, "file") for name in names])
            root = HARNESS.extract_corpus(archive, parent / "output", pin)
            imported = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())
            self.assertEqual(imported, sorted(names[:-1]))
            self.assertTrue(all((root / name).read_bytes() == b"sql" for name in imported))

    def test_checksum_failure_leaves_no_imported_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            archive, pin = self.make_archive(parent, [("postgresql-18.4/COPYRIGHT", "file")])
            pin["sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                HARNESS.extract_corpus(archive, parent / "output", pin)
            self.assertFalse((parent / "output").exists())

    def test_symlinks_traversal_duplicate_members_and_wrong_root_are_rejected(self) -> None:
        for members in (
            [("postgresql-18.4/src/test/regress/sql/link", "symlink")],
            [("postgresql-18.4/src/test/regress/../../escape", "file")],
            [("postgresql-18.4/COPYRIGHT", "file")] * 2,
            [("other/COPYRIGHT", "file")],
        ):
            with self.subTest(members=members), tempfile.TemporaryDirectory() as temporary:
                parent = Path(temporary)
                archive, pin = self.make_archive(parent, members)
                with self.assertRaises(ValueError):
                    HARNESS.extract_corpus(archive, parent / "output", pin)


class UpstreamResultTest(unittest.TestCase):
    def test_official_serial_parallel_and_failure_tap_is_understood(self) -> None:
        transcript = "# using postmaster on port 61722\nok 1     - setup              6 ms\nnot ok 2 + collate.utf8      28 ms\n1..2\n# 1 of 2 tests failed.\n"
        rows, errors = HARNESS.parse_tap(transcript, ["setup", "collate.utf8"], 1)
        self.assertEqual(errors, [])
        self.assertEqual(rows["setup"]["status"], "passed")
        self.assertEqual(rows["collate.utf8"], {"status": "failed", "milliseconds": 28})

    def test_zero_exit_cannot_hide_missing_duplicate_extra_or_skipped_tests(self) -> None:
        for transcript in (
            "",
            "ok 1 - a 1 ms\n1..1\n",
            "ok 1 - a 1 ms\nok 2 - a 1 ms\n1..2\n",
            "ok 1 - a 1 ms\nok 2 - b 1 ms\nok 3 - extra 1 ms\n1..3\n",
            "ok 1 - a 1 ms\nok 2 - b 1 ms # SKIP\n1..2\n",
            "ok 1 - a 1 ms\nok 2 - b 1 ms\n",
            "ok 1 - a 1 ms\nok 4 - b 1 ms\n1..2\n",
            "not ok 1 - a 1 ms\nok 2 - b 1 ms\n1..2\n",
            "ok 1 - a 1 ms\nok 2 - b 1 ms\n1..2\nBail out! server died\n",
        ):
            with self.subTest(transcript=transcript):
                _, errors = HARNESS.parse_tap(transcript, ["a", "b"], 0)
                self.assertTrue(errors)

    def test_infrastructure_exit_cannot_be_misreported_as_a_passing_run(self) -> None:
        for code in (1, 2, 124, -9):
            with self.subTest(code=code):
                _, errors = HARNESS.parse_tap("ok 1 - a 1 ms\n1..1\n", ["a"], code)
                self.assertTrue(errors)

    def test_partial_runs_keep_every_unexecuted_test_in_the_denominator(self) -> None:
        inventory = HARNESS.read_json(HERE / "inventory.json")
        report = HARNESS.initial_report(inventory, "postgres", "reference")
        report["tests"]["core/boolean"]["status"] = "passed"
        HARNESS.finish_report(report)
        self.assertEqual(report["counts"], {"passed": 1, "failed": 0, "not_run": 353})
        self.assertFalse(report["all_cases_passed"])
        self.assertEqual(report["kind"], "postgres_reference_validation")

    def test_a_stale_image_or_wrong_backend_cannot_supply_passing_evidence(self) -> None:
        inventory = HARNESS.read_json(HERE / "inventory.json")
        report = HARNESS.initial_report(inventory, "postgres", "reference")
        HARNESS.validate_report(report, inventory, "postgres")
        for field in ("harness_sha256", "inventory_sha256", "configuration_sha256", "backend", "source"):
            changed = copy.deepcopy(report)
            changed[field] = "stale"
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "differs from this checkout"):
                HARNESS.validate_report(changed, inventory, "postgres")
        del report["tests"]["isolation/prepared-transactions"]
        with self.assertRaisesRegex(ValueError, "every upstream test"):
            HARNESS.validate_report(report, inventory, "postgres")

    def test_runner_errors_prevent_success_even_when_every_test_passes(self) -> None:
        inventory = HARNESS.read_json(HERE / "inventory.json")
        report = HARNESS.initial_report(inventory, "uqa", "caller-revision")
        for row in report["tests"].values():
            row["status"] = "passed"
        report["errors"].append("inconsistent driver exit status")
        HARNESS.finish_report(report)
        self.assertFalse(report["all_cases_passed"])
        report["errors"].clear()
        report["container_exit_code"] = 2
        HARNESS.finish_report(report)
        self.assertFalse(report["all_cases_passed"])


class UpstreamTargetTest(unittest.TestCase):
    def args(self, **overrides) -> argparse.Namespace:
        data = dict(backend="uqa", host="engine", catalog_host="catalog-engine", port=5432, revision="a" * 40, suite="all", timeout=60, user="postgres")
        data.update(overrides)
        return argparse.Namespace(**data)

    def test_uqa_requires_explicit_targets_pinned_revision_and_independent_catalog_server(self) -> None:
        HARNESS.validate_target(self.args())
        for changes in ({"host": None}, {"revision": "main"}, {"catalog_host": None}, {"catalog_host": "engine"}, {"port": 0}, {"timeout": 0}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                HARNESS.validate_target(self.args(**changes))
        HARNESS.validate_target(self.args(suite="isolation", catalog_host=None))

    def test_reference_cannot_be_pointed_at_an_external_server_or_given_a_false_revision(self) -> None:
        args = self.args(backend="postgres", host=None, catalog_host=None, revision=None)
        HARNESS.validate_target(args)
        args.host = "existing-application-server"
        with self.assertRaisesRegex(ValueError, "fresh local clusters"):
            HARNESS.validate_target(args)

    def test_postgresql_cannot_be_reported_as_uqa(self) -> None:
        with mock.patch.object(HARNESS.subprocess, "check_output", return_value="18.4\n"):
            with self.assertRaisesRegex(ValueError, "does not identify as UQA"):
                HARNESS.verify_uqa_server(Path("psql"), "engine", self.args())
        with mock.patch.object(HARNESS.subprocess, "check_output", return_value="18.0-uqa\n"):
            self.assertEqual(HARNESS.verify_uqa_server(Path("psql"), "engine", self.args()), "18.0-uqa")


if __name__ == "__main__":
    unittest.main()
