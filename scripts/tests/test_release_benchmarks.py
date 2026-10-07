#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check report completeness, failed runs and the actual release attachment gate."""

from __future__ import annotations

import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_benchmarks", ROOT / "scripts/run-release-benchmarks.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def criterion_result(directory: Path, name: str, folder: str = "case") -> Path:
    output = directory / folder / "new"
    output.mkdir(parents=True)
    estimate = {"point_estimate": 12.0, "standard_error": 0.25,
                "confidence_interval": {"lower_bound": 11.0, "upper_bound": 13.0,
                                        "confidence_level": 0.95}}
    payloads = {
        "benchmark.json": {"full_id": name},
        "estimates.json": {"mean": estimate, "median": estimate},
        "sample.json": {"sampling_mode": "Linear", "iters": list(range(1, 11)),
                        "times": [12.0 * count for count in range(1, 11)]},
    }
    for filename, value in payloads.items():
        (output / filename).write_text(json.dumps(value))
    return output


def provenance() -> dict:
    return {
        "tag": "v1.2.3", "commit": "a" * 40, "repository": "cognica-io/uqa-engine",
        "run_id": "123", "run_attempt": "1", "run_url": "https://example.invalid/run/123",
        "profile": "bench", "features": [], "performance_acceptance": False,
        "runner": {"os": "fixture", "cpu": "fixture", "logical_cpus": 4},
    }


class CriterionReportsTest(unittest.TestCase):
    def test_reads_statistical_estimates_without_copying_individual_samples(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            criterion_result(directory, "queries/scan")
            result = RUNNER.collect_results(directory, ["queries/scan"])[0]
            self.assertEqual(result["unit"], "ns")
            self.assertEqual(result["samples"], 10)
            self.assertEqual(result["median"]["point_estimate"], 12.0)
            self.assertNotIn("times", result)
            self.assertNotIn("iters", result)

    def test_rejects_missing_and_unexpected_benchmarks(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            criterion_result(directory, "queries/scan")
            for expected in (["queries/scan", "queries/join"], ["queries/join"]):
                with self.subTest(expected=expected), self.assertRaisesRegex(RUNNER.BenchmarkError, "inventory differs"):
                    RUNNER.collect_results(directory, expected)

    def test_ignores_old_baselines_and_rejects_duplicate_identities(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            output = criterion_result(directory, "queries/scan")
            output.rename(output.with_name("base"))
            with self.assertRaisesRegex(RUNNER.BenchmarkError, "missing="):
                RUNNER.collect_results(directory, ["queries/scan"])
            criterion_result(directory, "queries/scan", "first")
            criterion_result(directory, "queries/scan", "second")
            with self.assertRaisesRegex(RUNNER.BenchmarkError, "duplicate benchmark"):
                RUNNER.collect_results(directory, ["queries/scan"])

    def test_rejects_truncated_samples_and_missing_estimates(self):
        for filename, value in (("sample.json", {"iters": [1], "times": [12]}),
                                ("estimates.json", {})):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                output = criterion_result(directory, "queries/scan")
                (output / filename).write_text(json.dumps(value))
                with self.assertRaises(RUNNER.BenchmarkError):
                    RUNNER.collect_results(directory, ["queries/scan"])

    def test_rejects_nonfinite_negative_and_inconsistent_estimates(self):
        for point in (float("nan"), float("inf"), -1, 100, True):
            with self.subTest(point=point), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                output = criterion_result(directory, "queries/scan")
                path = output / "estimates.json"
                payload = json.loads(path.read_text())
                payload["median"]["point_estimate"] = point
                path.write_text(json.dumps(payload))
                with self.assertRaises(RUNNER.BenchmarkError):
                    RUNNER.collect_results(directory, ["queries/scan"])


class ReleaseRunTest(unittest.TestCase):
    def test_complete_inventory_succeeds_and_records_each_provider(self):
        manifest = RUNNER.load_manifest()
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "output"
            binaries = {}
            for suite in manifest["suites"]:
                path = Path(temporary) / suite["target"]
                path.write_bytes(b"synthetic executable")
                binaries[suite["target"]] = path
            results = [{"name": "fixture", "unit": "ns", "samples": 10,
                        "mean": {"point_estimate": 12.0},
                        "median": {"point_estimate": 12.0, "confidence_interval": {
                            "lower_bound": 11.0, "upper_bound": 13.0, "confidence_level": 0.95}}}]
            with mock.patch.object(RUNNER, "provenance", return_value=provenance()), \
                    mock.patch.object(RUNNER, "run_command"), \
                    mock.patch.object(RUNNER, "executables", return_value=binaries), \
                    mock.patch.object(RUNNER, "collect_results", return_value=results):
                self.assertEqual(RUNNER.run("v1.2.3", output), 0)
            report = json.loads(next((output / "reports").glob("*.json")).read_text())
            self.assertEqual(report["status"], "complete")
            self.assertEqual({s["env"].get("UQA_STORAGE_BENCH_PROVIDER") for s in report["suites"]},
                             {None, "sqlite", "sqlite_kv", "redb"})
            self.assertFalse(report["provenance"]["performance_acceptance"])

    def test_builds_once_and_keeps_independent_providers_after_a_failure(self):
        manifest = RUNNER.load_manifest()
        commands = []
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = {}
            for suite in manifest["suites"]:
                binaries[suite["target"]] = root / suite["target"]
                binaries[suite["target"]].write_bytes(b"synthetic executable identity")

            def execute(command, log, *, env, timeout):
                commands.append((command, env))
                if command[0] == "cargo":
                    log.write_text("\n".join(json.dumps({
                        "reason": "compiler-artifact", "target": {"name": name, "kind": ["bench"]},
                        "executable": str(path),
                    }) for name, path in binaries.items()))
                    return
                suite = next(s for s in manifest["suites"] if s["name"] == log.stem)
                if suite["name"] == "sql_sqlite_kv":
                    raise RUNNER.BenchmarkError("fixture failure")
                for index, name in enumerate(suite["cases"]):
                    criterion_result(Path(env["CRITERION_HOME"]), name, str(index))

            with mock.patch.object(RUNNER, "provenance", return_value=provenance()), \
                    mock.patch.object(RUNNER, "run_command", side_effect=execute):
                self.assertEqual(RUNNER.run("v1.2.3", root / "output"), 1)
            report = json.loads(next((root / "output/reports").glob("*.json")).read_text())
            self.assertEqual(report["status"], "failed")
            statuses = {s["name"]: s["status"] for s in report["suites"]}
            self.assertEqual(statuses["sql_sqlite_kv"], "failed")
            self.assertEqual(statuses["sql_redb"], "complete")
            self.assertEqual(len(commands), 6)
            self.assertEqual(commands[0][0].count("--bench"), 1)
            self.assertEqual(commands[0][0][-1], "release_inventory")
            self.assertEqual(
                [env["UQA_RELEASE_BENCH_SUITE"] for _, env in commands[1:]],
                ["query_matrix", "sql_sqlite_e2e", "sql_sqlite_e2e", "sql_sqlite_e2e", "retrieval_workloads"],
            )
            homes = [env["CRITERION_HOME"] for _, env in commands[1:]]
            self.assertEqual(len(set(homes)), 5)
            summary = next((root / "output/reports").glob("*.md")).read_text()
            self.assertIn("fixture failure", summary)
            self.assertIn("does not replace", summary)

    def test_build_failure_still_produces_an_incomplete_inventory(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(RUNNER, "provenance", return_value=provenance()), \
                mock.patch.object(RUNNER, "run_command", side_effect=RUNNER.BenchmarkError("build failed")) as command:
            output = Path(temporary) / "output"
            self.assertEqual(RUNNER.run("v1.2.3", output), 1)
            self.assertEqual(command.call_count, 1)
            report = json.loads(next((output / "reports").glob("*.json")).read_text())
            self.assertEqual(report["status"], "failed")
            self.assertTrue(all(s["status"] == "not_run" for s in report["suites"]))

    def test_refuses_to_mix_a_new_run_with_existing_results(self):
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(RUNNER, "provenance", return_value=provenance()), \
                mock.patch.object(RUNNER, "run_command") as command:
            with self.assertRaises(FileExistsError):
                RUNNER.run("v1.2.3", Path(temporary))
            command.assert_not_called()

    def test_rejects_a_checkout_different_from_the_tag_before_building(self):
        with mock.patch.object(RUNNER, "command_text", side_effect=["a" * 40, "b" * 40]):
            with self.assertRaisesRegex(RUNNER.BenchmarkError, "does not match the release tag"):
                RUNNER.provenance("v1.2.3", RUNNER.load_manifest())


class ReportPublicationTest(unittest.TestCase):
    def publication_check(self, sources: list[dict], *, orphan: bool = False):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        step = workflow.split("      - name: Verify report release identities\n", 1)[1].split("\n      - ", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "benchmark-reports").mkdir()
            gh = root / "gh"
            gh.write_text("#!/bin/sh\nprintf '%s\\n' " + "a" * 40 + "\n")
            gh.chmod(0o755)
            for source in sources:
                name = f"release-benchmarks-{source['tag']}-{source['run_id']}-{source['run_attempt']}"
                path = root / "benchmark-reports" / name
                Path(str(path) + ".json").write_text(json.dumps({"schema_version": 1, "provenance": source}))
                Path(str(path) + ".md").write_text("Incomplete run recorded.")
            if orphan:
                (root / "benchmark-reports/orphan.md").write_text("not a report")
            return subprocess.run(["bash", "-e"], input=script, text=True, cwd=root,
                                  capture_output=True, check=False, env={
                                      **os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                                      "RELEASE_TAG": "v1.2.3", "GITHUB_RUN_ID": "123",
                                      "GITHUB_REPOSITORY": "cognica-io/uqa-engine",
                                  })

    def test_keeps_each_attempt_and_allows_failed_measurements_to_be_recorded(self):
        first = provenance()
        second = {**first, "run_attempt": "2"}
        result = self.publication_check([first, second])
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_wrong_tag_commit_repository_and_workflow(self):
        for key, value in (("tag", "v1.2.4"), ("commit", "b" * 40),
                           ("repository", "elsewhere/project"), ("run_id", "999"),
                           ("performance_acceptance", True)):
            with self.subTest(key=key):
                source = copy.deepcopy(provenance())
                source[key] = value
                result = self.publication_check([source])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("provenance differs", result.stderr)

    def test_rejects_absent_and_unpaired_reports(self):
        for sources, orphan in (([], False), ([provenance()], True)):
            with self.subTest(orphan=orphan):
                self.assertNotEqual(self.publication_check(sources, orphan=orphan).returncode, 0)


if __name__ == "__main__":
    unittest.main()
