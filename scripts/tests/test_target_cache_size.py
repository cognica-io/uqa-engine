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


if __name__ == "__main__":
    unittest.main()
