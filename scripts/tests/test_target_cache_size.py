#!/usr/bin/env python3
"""Check disk-usage boundaries and failures without allocating a huge cache."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("target_cache_size", ROOT / "scripts/check-target-cache-size.py")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class TargetCacheSizeTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(prefix="uqa-cache-limit-")
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        patcher = mock.patch.object(CHECKER, "ROOT", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)
        discovery = mock.patch.object(CHECKER, "target_directories", return_value=[self.root / "target"])
        discovery.start()
        self.addCleanup(discovery.stop)

    def test_missing_target_does_not_run_du(self) -> None:
        with mock.patch.object(CHECKER.subprocess, "run") as run:
            self.assertEqual(CHECKER.allocated_bytes(self.root / "target"), 0)
        run.assert_not_called()

    def test_linked_target_counts_real_allocated_data_once(self) -> None:
        data = self.root / "cache with spaces"
        data.mkdir()
        (data / "artifact").write_bytes(b"x" * 16_384)
        os.link(data / "artifact", data / "hardlink")
        target = self.root / "target"
        target.symlink_to(data, target_is_directory=True)
        used = CHECKER.allocated_bytes(target)
        self.assertEqual(used, CHECKER.allocated_bytes(data))
        self.assertGreaterEqual(used, 16_384)
        (data / "hardlink").unlink()
        self.assertEqual(CHECKER.allocated_bytes(target), used)

    def test_broken_target_symlink_blocks_the_commit(self) -> None:
        (self.root / "target").symlink_to(self.root / "missing")
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertEqual(CHECKER.main(), 1)
        self.assertIn("commit blocked", errors.getvalue())

    def test_du_failures_and_invalid_output_block_the_commit(self) -> None:
        (self.root / "target").mkdir()
        failures = [
            OSError("du is unavailable"),
            subprocess.CalledProcessError(1, ["du"], stderr="Permission denied"),
        ]
        outputs = ["", "-1\ttarget\n", "size\ttarget\n", "1\n", "1\ttarget\n2\tother\n"]
        for failure in failures:
            with self.subTest(failure=failure), mock.patch.object(CHECKER.subprocess, "run", side_effect=failure):
                with contextlib.redirect_stderr(io.StringIO()) as errors:
                    self.assertEqual(CHECKER.main(), 1)
                self.assertIn("commit blocked", errors.getvalue())
        for output in outputs:
            result = subprocess.CompletedProcess(["du"], 0, stdout=output)
            with self.subTest(output=output), mock.patch.object(CHECKER.subprocess, "run", return_value=result):
                with contextlib.redirect_stderr(io.StringIO()) as errors:
                    self.assertEqual(CHECKER.main(), 1)
                self.assertIn("commit blocked", errors.getvalue())


class SharedTargetCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(prefix="uqa-shared-cache-limit-")
        self.addCleanup(directory.cleanup)
        self.directory = pathlib.Path(directory.name).resolve()
        self.primary = self.directory / "primary repo"
        self.linked = self.directory / "linked worktree"
        self.sibling = self.directory / "independent worktree"
        self.environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith("GIT_") and key != "CARGO_TARGET_DIR"
        }
        self.environment.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "--quiet", str(self.primary))
        self.git(
            "-C", str(self.primary), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "--quiet", "--allow-empty", "-m", "Fixture",
        )
        self.git("-C", str(self.primary), "worktree", "add", "--quiet", "--detach", str(self.linked))
        self.git("-C", str(self.primary), "worktree", "add", "--quiet", "--detach", str(self.sibling))
        root = mock.patch.object(CHECKER, "ROOT", self.linked)
        root.start()
        self.addCleanup(root.stop)
        environment = mock.patch.dict(os.environ, self.environment, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def git(self, *arguments: str) -> None:
        subprocess.run(["git", *arguments], check=True, capture_output=True, env=self.environment)

    def test_linked_worktree_cannot_hide_an_oversized_primary_target(self) -> None:
        targets = CHECKER.target_directories()
        self.assertEqual(set(targets), {self.primary / "target", self.linked / "target"})

        def size(target: pathlib.Path) -> int:
            return CHECKER.LIMIT_BYTES + 1024 if target == self.primary / "target" else 0

        with mock.patch.object(CHECKER, "allocated_bytes", side_effect=size):
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                self.assertEqual(CHECKER.main(), 1)
        self.assertIn(str(self.primary / "target"), errors.getvalue())

    def test_explicit_relative_target_and_aliases_are_checked(self) -> None:
        for configured in ["../external cache", str(self.primary / "target"), str(self.sibling / "target")]:
            with self.subTest(configured=configured), mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": configured}):
                targets = CHECKER.target_directories()
                expected = pathlib.Path(os.path.abspath(self.linked / configured))
                self.assertIn(expected, targets)
                self.assertEqual(targets.count(expected), 1)
                with mock.patch.object(CHECKER, "allocated_bytes", side_effect=lambda target: CHECKER.LIMIT_BYTES + 1024 if target == expected else 0):
                    with contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(CHECKER.main(), 1)

    def test_size_boundary_and_git_discovery_errors_remain_fail_closed(self) -> None:
        with mock.patch.object(CHECKER, "allocated_bytes", return_value=CHECKER.LIMIT_BYTES):
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(CHECKER.main(), 0)
        for result in ["", "worktree relative\0", subprocess.CalledProcessError(1, ["git"])]:
            arguments = {"side_effect": result} if isinstance(result, Exception) else {"return_value": result}
            with self.subTest(result=result), mock.patch.object(CHECKER.subprocess, "check_output", **arguments):
                with contextlib.redirect_stderr(io.StringIO()) as errors:
                    self.assertEqual(CHECKER.main(), 1)
                self.assertIn("commit blocked", errors.getvalue())


if __name__ == "__main__":
    unittest.main()
