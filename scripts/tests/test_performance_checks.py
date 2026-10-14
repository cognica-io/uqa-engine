#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import importlib.util
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import time
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
            "case_names": ["memory", "sqlite"],
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

    def test_case_names_must_be_complete_distinct_and_valid(self):
        for names in (None, [], ["memory"], ["memory", "memory"], ["memory", "sqlite/extra"]):
            self.check["case_names"] = names
            with self.subTest(names=names), self.assertRaisesRegex(ValueError, "expected case name"):
                self.write_inventory()

    def test_rust_case_names_keep_uppercase_identifiers(self):
        self.check["case_names"] = ["backend_1::COMMIT", "backend_1::ROLLBACK"]
        checks = self.write_inventory()
        self.assertEqual(checks[0]["case_names"], self.check["case_names"])

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

    def test_replaced_parameter_case_fails_even_with_the_same_count(self):
        self.suite["testcases"]["tests::bounded_work::different_provider"] = self.suite["testcases"].pop("tests::bounded_work::sqlite")
        with self.assertRaisesRegex(ValueError, "case identities differ"):
            runner.verify_selection([self.check], self.listing)

    def test_arguments_select_only_existing_test_targets(self):
        args = runner.arguments([self.check, {**self.check, "test": "other"}])
        self.assertEqual(args.count("-p"), 1)
        self.assertEqual(args[:2], ["--profile", "ci"])
        self.assertIn("--locked", args)
        self.assertIn("--lib", args)
        self.assertIn("--tests", args)
        self.assertNotIn("--all-targets", args)
        self.assertIn("test(/(^|::)bounded_work(::.*)?$/)", args[-1])

    def test_archive_selection_preserves_inventory_without_cargo_build_flags(self):
        archive = self.root / "tests.tar.zst"
        args = runner.arguments([self.check], archive)
        self.assertEqual(args[:2], ["--profile", "ci"])
        self.assertEqual(args[args.index("--archive-file") + 1], str(archive.resolve()))
        self.assertEqual(args[args.index("--workspace-remap") + 1], str(self.root))
        self.assertEqual(args[-2:], runner.arguments([self.check])[-2:])
        for flag in ("--locked", "--lib", "--tests", "-p"):
            self.assertNotIn(flag, args)

    def test_archive_build_compiles_every_inventory_owner_without_executing_tests(self):
        archive = self.root / "tests.tar.zst"
        with (
            mock.patch.object(runner, "inventory", return_value=[self.check]),
            mock.patch.object(runner, "capture") as capture,
            mock.patch.object(runner.subprocess, "run") as run,
            mock.patch("sys.argv", ["run-performance-checks", "--build-archive", str(archive)]),
        ):
            self.assertEqual(runner.main(), 0)
        capture.assert_not_called()
        run.assert_called_once_with([
            "cargo", "nextest", "archive", "--profile", "ci", "--locked", "--lib",
            "--tests", "-p", "uqa-example", "--archive-file", str(archive),
        ], cwd=self.root, check=True)

    def test_failure_report_retains_diagnostic_and_does_not_claim_timing_acceptance(self):
        output = self.root / "output"
        result = {"status": "failed", "revision": "a" * 40, "checks": [], "timing_acceptance": False, "error": "missing provider case"}
        with mock.patch.dict("os.environ", {"GITHUB_STEP_SUMMARY": str(self.root / "step.md")}):
            runner.report(output, result)
        self.assertEqual(json.loads((output / "summary.json").read_text()), result)
        self.assertIn("**failed**", (output / "summary.md").read_text())
        self.assertIn("missing provider case", (self.root / "step.md").read_text())

    def run_main(self, *, listing=None, listing_failure=None, failure=None, archive=None):
        self.write_inventory()
        output = self.root / "run-output"
        output.mkdir(parents=True, exist_ok=True)
        (output / "test-inventory.json").write_text("stale inventory")
        junit = self.root / "target/nextest/ci/junit.xml"
        junit.parent.mkdir(parents=True, exist_ok=True)
        junit.write_text("stale result")
        with (
            mock.patch.object(runner, "MANIFEST", self.manifest),
            mock.patch.object(runner, "inventory", return_value=[self.check]),
            mock.patch.object(runner, "capture", side_effect=["a" * 40, "rustc", "nextest", listing_failure or json.dumps(self.listing if listing is None else listing)]) as capture,
            mock.patch.object(runner.subprocess, "run", side_effect=failure) as run,
            mock.patch("sys.argv", ["run-performance-checks", "--output", str(output)] + (
                ["--archive-file", str(archive)] if archive is not None else []
            )),
            mock.patch.dict("os.environ", {}, clear=True),
            mock.patch("sys.stderr"),
        ):
            status = runner.main()
        self.assertFalse(junit.exists())
        return status, json.loads((output / "summary.json").read_text()), capture, run

    def test_success_runs_exactly_the_validated_selection(self):
        status, result, capture, run = self.run_main()
        self.assertEqual(status, 0)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["checks"][0]["selected_cases"], 2)
        self.assertEqual(list(capture.call_args.args[5:]), runner.arguments([self.check]))
        self.assertEqual(run.call_args.args[0][3:], runner.arguments([self.check]))
        self.assertTrue(run.call_args.kwargs["check"])
        self.assertIsNone(capture.call_args.kwargs["timeout"])
        self.assertEqual(json.loads((self.root / "run-output/test-inventory.json").read_text()), self.listing)

    def test_test_process_failure_stays_failed_and_retains_summary(self):
        status, result, _, _ = self.run_main(failure=subprocess.CalledProcessError(100, ["cargo", "nextest", "run"]))
        self.assertEqual(status, 1)
        self.assertEqual(result["status"], "failed")
        self.assertIn("100", result["error"])

    def test_archived_run_verifies_exact_cases_and_retains_test_failure(self):
        archive = self.root / "tests.tar.zst"
        status, result, capture, run = self.run_main(
            archive=archive, failure=subprocess.CalledProcessError(100, ["cargo", "nextest", "run"]),
        )
        self.assertEqual(status, 1)
        self.assertEqual(result["checks"][0]["selected_cases"], 2)
        self.assertEqual(list(capture.call_args.args[5:]), runner.arguments([self.check], archive))
        self.assertEqual(run.call_args.args[0][3:], runner.arguments([self.check], archive))
        self.assertIn("100", result["error"])
        self.assertEqual(capture.call_args.kwargs["timeout"], 120)

    def test_archived_run_rejects_missing_provider_cases_before_execution(self):
        status, result, _, run = self.run_main(
            archive=self.root / "tests.tar.zst", listing={"rust-suites": {}},
        )
        self.assertEqual(status, 1)
        self.assertEqual(result["status"], "failed")
        run.assert_not_called()

    def test_selection_failure_never_runs_a_partial_suite(self):
        status, result, _, run = self.run_main(listing={"rust-suites": {}})
        self.assertEqual(status, 1)
        self.assertEqual(result["status"], "failed")
        run.assert_not_called()

    def test_discovery_timeout_retains_failed_report_and_does_not_run_tests(self):
        status, result, _, run = self.run_main(
            archive=self.root / "tests.tar.zst",
            listing_failure=subprocess.TimeoutExpired(["cargo", "nextest", "list"], 120),
        )
        self.assertEqual(status, 1)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["stage"], "discovery")
        self.assertIn("timed out", result["error"])
        self.assertFalse((self.root / "run-output/test-inventory.json").exists())
        run.assert_not_called()

    def test_discovery_interruption_retains_failed_report_and_does_not_run_tests(self):
        for failure, name in [(KeyboardInterrupt(), "SIGINT"), (InterruptedError("discovery interrupted by SIGTERM"), "SIGTERM")]:
            with self.subTest(signal=name):
                status, result, _, run = self.run_main(
                    archive=self.root / "tests.tar.zst", listing_failure=failure,
                )
                self.assertEqual(status, 1)
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["stage"], "discovery")
                self.assertIn(name, result["error"])
                self.assertFalse((self.root / "run-output/test-inventory.json").exists())
                run.assert_not_called()

    def test_bounded_capture_preserves_output_and_nonzero_exit(self):
        self.assertEqual(runner.capture(sys.executable, "-c", "print('exact inventory')", timeout=5), "exact inventory")
        with self.assertRaises(subprocess.CalledProcessError) as failure:
            runner.capture(sys.executable, "-c", "print('failed inventory'); raise SystemExit(7)", timeout=5)
        self.assertEqual(failure.exception.returncode, 7)
        self.assertEqual(failure.exception.output, "failed inventory")

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_interrupted_listing_cleans_up_its_process_group(self):
        process = mock.Mock(pid=42)
        process.wait.side_effect = [KeyboardInterrupt(), None]
        with mock.patch.object(runner.subprocess, "Popen", return_value=process), mock.patch.object(runner.os, "killpg") as kill:
            with self.assertRaises(KeyboardInterrupt):
                runner.capture("cargo", "nextest", "list", timeout=120)
        kill.assert_called_once_with(42, signal.SIGKILL)

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_signals_stop_listing_descendants_and_restore_prior_handlers(self):
        child_script = (
            "import os,pathlib,time\n"
            "pathlib.Path('parent.pid').write_text(str(os.getpid()))\n"
            "if os.fork() == 0:\n"
            " pathlib.Path('child.pid').write_text(str(os.getpid()))\n"
            "time.sleep(30)\n"
        )
        for signum in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=signum.name), tempfile.TemporaryFile(mode="w+") as output:
                directory = self.root / signum.name
                directory.mkdir()
                script = (
                    "import importlib.util,pathlib,signal,sys\n"
                    f"spec=importlib.util.spec_from_file_location('runner', {str(ROOT / 'scripts/run-performance-checks.py')!r})\n"
                    "runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)\n"
                    f"runner.ROOT=pathlib.Path({str(directory)!r})\n"
                    "previous={s:signal.getsignal(s) for s in (signal.SIGINT,signal.SIGTERM)}\n"
                    "try:\n"
                    f" runner.capture(sys.executable,'-c',{child_script!r},timeout=20)\n"
                    "except InterruptedError as error:\n"
                    " assert all(signal.getsignal(s)==handler for s,handler in previous.items())\n"
                    " pathlib.Path('interruption.txt').write_text(str(error))\n"
                    " raise SystemExit(23)\n"
                )
                process = subprocess.Popen([sys.executable, "-c", script], cwd=directory, stdout=output, stderr=subprocess.STDOUT)
                try:
                    deadline = time.monotonic() + 10
                    while not (directory / "child.pid").exists() and time.monotonic() < deadline:
                        self.assertIsNone(process.poll())
                        time.sleep(0.01)
                    self.assertTrue((directory / "child.pid").exists())
                    process.send_signal(signum)
                    process.wait(timeout=10)
                    output.seek(0)
                    self.assert_processes_stopped([int((directory / name).read_text()) for name in ("parent.pid", "child.pid")])
                    self.assertEqual(process.returncode, 23, output.read())
                    self.assertIn(signum.name, (directory / "interruption.txt").read_text())
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
                    parent = directory / "parent.pid"
                    if parent.exists():
                        try:
                            os.killpg(int(parent.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def assert_processes_stopped(self, pids):
        for pid in pids:
            deadline = time.monotonic() + 5
            while True:
                state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], text=True, capture_output=True, timeout=1).stdout.strip()
                if not state or state.startswith("Z") or time.monotonic() >= deadline:
                    break
                time.sleep(0.01)
            self.assertTrue(not state or state.startswith("Z"), (pid, state))

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_bounded_capture_terminates_the_listing_and_its_descendant(self):
        script = (
            "import os,pathlib,time\n"
            "pathlib.Path('parent.pid').write_text(str(os.getpid()))\n"
            "if os.fork() == 0:\n"
            " pathlib.Path('child.pid').write_text(str(os.getpid()))\n"
            "time.sleep(30)\n"
        )
        try:
            with self.assertRaises(subprocess.TimeoutExpired):
                runner.capture(sys.executable, "-c", script, timeout=3)
            parent = int((self.root / "parent.pid").read_text())
            child = int((self.root / "child.pid").read_text())
            self.assert_processes_stopped([parent, child])
        finally:
            parent_path = self.root / "parent.pid"
            if parent_path.exists():
                try:
                    os.killpg(int(parent_path.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    unittest.main()
