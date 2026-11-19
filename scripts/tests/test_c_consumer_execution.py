#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from contextlib import redirect_stdout
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("c_consumers", ROOT / "scripts/c_consumer_execution.py")
CONSUMERS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONSUMERS)


class ConsumerExecutionTests(unittest.TestCase):
    def test_processes_overlap_within_the_limit_and_keep_separate_environments(self):
        child = """
import os
from pathlib import Path
import time
directory = Path(os.environ['TMPDIR'])
assert os.environ['UQA_TEST_DATABASE_DIR'] == str(directory)
provider = os.environ['UQA_TEST_STORAGE']
(directory / 'started').write_text(provider)
deadline = time.monotonic() + 15
while not (directory.parent / 'release').exists():
    if time.monotonic() >= deadline:
        raise RuntimeError('parent did not release consumers')
    time.sleep(0.01)
(directory / 'finished').write_text(provider)
"""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cases = []
            for provider in ("memory", "sqlite_native", "sqlite_kv", "redb"):
                directory = root / provider
                directory.mkdir()
                environment = dict(os.environ, TMPDIR=str(directory),
                                   UQA_TEST_DATABASE_DIR=str(directory), UQA_TEST_STORAGE=provider)
                cases.append((provider, [sys.executable, "-c", child], environment))
            output = io.StringIO()
            with redirect_stdout(output), ThreadPoolExecutor(max_workers=1) as executor:
                future = executor.submit(CONSUMERS.run_cases, cases, 2)
                try:
                    deadline = time.monotonic() + 10
                    while len(list(root.glob("*/started"))) < 2 and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertEqual(len(list(root.glob("*/started"))), 2)
                    self.assertFalse(list(root.glob("*/finished")))
                finally:
                    (root / "release").touch()
                self.assertEqual(future.result(timeout=10), 4)
            for provider, _, _ in cases:
                self.assertEqual((root / provider / "finished").read_text(), provider)
                self.assertIn(f"Passed {provider}", output.getvalue())

    def test_exit_and_spawn_failures_remain_failures_after_other_consumers_finish(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cases = []
            for name, code in (("first", 0), ("fails", 7), ("last", 0)):
                child = "from pathlib import Path; import sys; Path(sys.argv[1]).touch(); sys.exit(int(sys.argv[2]))"
                cases.append((name, [sys.executable, "-c", child, str(root / name), str(code)], dict(os.environ)))
            cases.append(("missing", [str(root / "missing-executable")], dict(os.environ)))
            output = io.StringIO()
            with redirect_stdout(output), self.assertRaisesRegex(RuntimeError, "fails, missing"):
                CONSUMERS.run_cases(cases, 2)
            self.assertTrue(all((root / name).is_file() for name in ("first", "fails", "last")))
            self.assertIn("Passed last", output.getvalue())
            self.assertIn("Failed fails (exit 7)", output.getvalue())
            self.assertIn("Failed missing", output.getvalue())

    def test_invalid_parallelism_is_rejected_before_compiling_or_running_consumers(self):
        with self.assertRaisesRegex(ValueError, "No native consumers"):
            CONSUMERS.run_cases([], 1)
        for jobs in (0, -1):
            with self.assertRaises(ValueError):
                CONSUMERS.run_cases([], jobs)
            for script in ("verify-c-clients.py", "verify-c-examples.py"):
                with self.subTest(jobs=jobs, script=script):
                    result = subprocess.run([sys.executable, str(ROOT / "scripts" / script),
                                             "--library", "missing-library", "--jobs", str(jobs)],
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("--jobs must be positive", result.stderr)


if __name__ == "__main__":
    unittest.main()
