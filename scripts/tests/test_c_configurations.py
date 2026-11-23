#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

from contextlib import redirect_stdout
import importlib.util
import io
import itertools
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("c_configurations", ROOT / "scripts/verify-c-configurations.py")
CONFIGURATIONS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONFIGURATIONS)


class ConfigurationTests(unittest.TestCase):
    def test_groups_cover_each_feature_combination_once(self):
        features = ("arrow-export", "arrow", "duckdb")
        expected = {frozenset(combination) for size in range(4)
                    for combination in itertools.combinations(features, size)}
        configurations = CONFIGURATIONS.GROUPS["core"] + CONFIGURATIONS.GROUPS["duckdb"]
        self.assertEqual(CONFIGURATIONS.GROUPS["consumers"], configurations)
        actual = [frozenset(name.split(",")) if name != "minimal" else frozenset()
                  for name in configurations if name != "default"]
        self.assertEqual(len(configurations), 9)
        self.assertEqual(len(actual), len(set(actual)))
        self.assertEqual(set(actual), expected)
        self.assertEqual(configurations.count("default"), 1)
        self.assertTrue(all("duckdb" not in name for name in CONFIGURATIONS.GROUPS["core"]))
        self.assertTrue(all("duckdb" in name for name in CONFIGURATIONS.GROUPS["duckdb"]))

    def test_workflow_matrix_preserves_every_platform_and_group_selection(self):
        linux, macos = "ubuntu-24.04", "macos-26-arm64"
        ordinary = [{"os": linux, "group": "consumers"}, {"os": macos, "group": "core"},
                    {"os": macos, "group": "duckdb"}]
        native = [{"os": linux, "configuration": "minimal"}, {"os": linux, "configuration": "arrow-export"},
                  {"os": macos, "configuration": "all"}]
        for platform, group in itertools.product(("all", linux, macos), ("all", "core", "duckdb", "sanitizers")):
            with self.subTest(platform=platform, group=group):
                matrices = json.loads(subprocess.check_output([
                    sys.executable, str(ROOT / "scripts/verify-c-configurations.py"), "--matrix", platform, group,
                ]))
                consumers = ordinary if group == "all" else []
                if group in ("core", "duckdb"):
                    consumers = [{"os": os_name, "group": group} for os_name in (linux, macos)]
                sanitizers = native if group in ("all", "sanitizers") else []
                for key, rows in (("consumers", consumers), ("sanitizers", sanitizers)):
                    expected = [row for row in rows if platform in ("all", row["os"])]
                    self.assertEqual(matrices[key], expected)

    def test_native_job_executes_its_selected_feature_graph(self):
        for selected in ("minimal", "arrow-export", "all"):
            args = ["verify-c-configurations.py", "--group", "sanitizers", "--configuration", selected]
            target = "aarch64-apple-darwin" if selected == "all" else "x86_64-unknown-linux-gnu"
            with self.subTest(selected=selected), patch.object(sys, "argv", args), \
                    patch.object(subprocess, "check_output", return_value=f"host: {target}\n"), \
                    patch.object(subprocess, "run") as execute, patch.dict(os.environ, {}, clear=True), \
                    redirect_stdout(io.StringIO()):
                self.assertEqual(CONFIGURATIONS.main(), 0)
                features = ("minimal", "arrow-export") if selected == "all" else (selected,)
                expected = [command for name in features for command in CONFIGURATIONS.commands(name, target=target)]
                self.assertEqual([call.args[0] for call in execute.call_args_list], expected)

    def test_each_build_selects_its_exact_features_before_running_consumers(self):
        for group in ("core", "duckdb"):
            for name in CONFIGURATIONS.GROUPS[group]:
                with self.subTest(group=group, name=name):
                    commands = CONFIGURATIONS.commands(name)
                    build, test = commands[:2]
                    self.assertEqual(build[:2], ["cargo", "build"])
                    self.assertEqual(test[:2], ["cargo", "test"])
                    for command in (build, test):
                        self.assertEqual("--no-default-features" in command, name != "default")
                        requested = {command[index + 1] for index, value in enumerate(command)
                                     if value == "--features"}
                        self.assertEqual(requested, {"test-support"} if name in ("minimal", "default")
                                         else {"test-support", name})
                    scripts = [Path(command[1]).name for command in commands if command[0] != "cargo"]
                    self.assertEqual(scripts.count("verify-c-clients.py"), 2)
                    self.assertEqual(scripts.count("verify-c-examples.py"), 2)
                    self.assertEqual(scripts.count("verify-c-xml-isolation.py"), 1)
                    self.assertEqual(sum("--sanitize" in command for command in commands), 2)
                    self.assertEqual(sum("test_support::tests" in command for command in commands),
                                     int(name == "minimal"))

    def test_native_sanitizers_keep_both_features_and_instrument_every_consumer(self):
        self.assertEqual(set(CONFIGURATIONS.GROUPS["sanitizers"]), {"minimal", "arrow-export"})
        targets = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
        for name, target in itertools.product(CONFIGURATIONS.GROUPS["sanitizers"], targets):
            with self.subTest(name=name, target=target), patch.dict(os.environ, {"CARGO_TARGET_DIR": "/tmp/c-target"}):
                commands = CONFIGURATIONS.commands(name, target=target)
                self.assertEqual("--tests" in commands[0], target == "x86_64-unknown-linux-gnu")
                if target == "x86_64-unknown-linux-gnu":
                    self.assertIn("--lib", commands[0])
                self.assertIn("--lib", commands[1])
                for command in commands[:2]:
                    self.assertEqual(command[:2], ["cargo", "+nightly"])
                    self.assertIn("-Zbuild-std", command)
                    self.assertEqual(command[command.index("--target") + 1], target)
                self.assertEqual(len(commands[2:]), 3)
                for command in commands[2:]:
                    self.assertIn("--sanitize", command)
                    self.assertEqual(Path(command[command.index("--library") + 1]).parent,
                                     Path("/tmp/c-target") / target / "debug")

    def test_failed_build_never_runs_stale_consumers_and_other_configurations_still_run(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if len(calls) == 1:
                raise subprocess.CalledProcessError(7, command)

        with tempfile.TemporaryDirectory() as temporary:
            summary = Path(temporary) / "summary.md"
            with patch.object(CONFIGURATIONS.subprocess, "run", side_effect=execute), \
                    patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}), \
                    redirect_stdout(io.StringIO()):
                self.assertEqual(CONFIGURATIONS.run("consumers"), 1)
            self.assertEqual(calls[0], CONFIGURATIONS.commands("minimal")[0])
            self.assertEqual(calls[1:], [command for name in CONFIGURATIONS.GROUPS["consumers"][1:]
                                        for command in CONFIGURATIONS.commands(name)])
            self.assertIn("| minimal | FAIL |", summary.read_text())
            self.assertEqual(summary.read_text().count("| PASS |"), 8)

    def test_failed_consumer_is_not_hidden_by_later_success(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if len(calls) == 3:
                raise subprocess.CalledProcessError(9, command)

        with patch.object(CONFIGURATIONS.subprocess, "run", side_effect=execute), \
                patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
            self.assertEqual(CONFIGURATIONS.run("sanitizers", target="native-host"), 1)
            expected = (CONFIGURATIONS.commands("minimal", target="native-host")[:3]
                        + CONFIGURATIONS.commands("arrow-export", target="native-host"))
        self.assertEqual(calls, expected)

    def test_success_requires_every_command_in_every_configuration(self):
        with patch.object(CONFIGURATIONS.subprocess, "run") as execute, \
                patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
            self.assertEqual(CONFIGURATIONS.run("duckdb"), 0)
            expected = [command for name in CONFIGURATIONS.GROUPS["duckdb"]
                        for command in CONFIGURATIONS.commands(name)]
        self.assertEqual([call.args[0] for call in execute.call_args_list], expected)
        self.assertTrue(all(call.kwargs == {"cwd": ROOT, "check": True}
                            for call in execute.call_args_list))


if __name__ == "__main__":
    unittest.main()
