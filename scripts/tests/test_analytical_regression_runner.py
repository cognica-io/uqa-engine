#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import os
import pathlib
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "run-analytical-regression.py"


def load_runner():
    specification = importlib.util.spec_from_file_location("analytical_regression", RUNNER)
    assert specification is not None
    assert specification.loader is not None
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


class AnalyticalRegressionRunnerTest(unittest.TestCase):
    def test_base_and_head_builds_use_revision_isolated_targets(self) -> None:
        runner = load_runner()
        root = pathlib.Path("/tmp/analytical-builds")
        base_revision = "a" * 40
        head_revision = "b" * 40

        base = runner.build_target_path(root, "base", base_revision)
        head = runner.build_target_path(root, "head", head_revision)

        self.assertEqual(base, root / "base" / base_revision)
        self.assertEqual(head, root / "head" / head_revision)
        self.assertNotEqual(base, head)

    def test_unknown_build_role_is_rejected(self) -> None:
        runner = load_runner()
        with self.assertRaisesRegex(ValueError, "unknown analytical build role"):
            runner.build_target_path(pathlib.Path("/tmp"), "shared", "a" * 40)

    def test_measure_invokes_criterion_benchmark_mode(self) -> None:
        runner = load_runner()
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            executable = root / "benchmark"
            criterion_home = root / "criterion"
            with mock.patch.object(runner, "run") as run, mock.patch("builtins.print"):
                runner.measure(executable, root, criterion_home, "head pair 1")

            run.assert_called_once()
            args, kwargs = run.call_args
            self.assertEqual(args, (str(executable), "--bench", "--noplot"))
            self.assertEqual(kwargs["cwd"], root)
            self.assertEqual(kwargs["env"]["CRITERION_HOME"], str(criterion_home))
            self.assertEqual(kwargs["env"].keys(), os.environ.keys() | {"CRITERION_HOME"})


if __name__ == "__main__":
    unittest.main()
