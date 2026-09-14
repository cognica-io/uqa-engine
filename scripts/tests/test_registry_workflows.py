#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the actual pre-publication integrity checks in both registry jobs."""

from __future__ import annotations

import hashlib
import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOWS = ("pypi.yml", "npmjs.yml")


class RegistryTransferIntegrityTest(unittest.TestCase):
    def run_check(self, workflow: str, files: dict[str, bytes], digests: str):
        source = (ROOT / ".github" / "workflows" / workflow).read_text()
        step = source.split(
            "      - name: Verify the transferred release files\n", 1
        )[1].split("\n      - ", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as temporary:
            for filename, content in files.items():
                (pathlib.Path(temporary) / filename).write_bytes(content)
            return subprocess.run(
                ["bash", "-e"],
                input=script,
                text=True,
                capture_output=True,
                cwd=temporary,
                env={**os.environ, "RELEASE_DIGESTS": digests},
                check=False,
            )

    @staticmethod
    def manifest(files: dict[str, bytes]) -> str:
        return "\n".join(
            f"{hashlib.sha256(content).hexdigest()}  {filename}"
            for filename, content in files.items()
        )

    def test_accepts_only_the_complete_verified_payload(self):
        files = {"uqa-0.3.0.tar.gz": b"source archive", "uqa.whl": b"wheel"}
        for workflow in WORKFLOWS:
            with self.subTest(workflow=workflow):
                result = self.run_check(workflow, files, self.manifest(files))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Verified all 2 transferred release files", result.stdout)

    def test_rejects_truncated_and_changed_archive_bytes(self):
        files = {"uqa-0.3.0.tar.gz": b"complete source archive"}
        for workflow in WORKFLOWS:
            for content in (b"", b"complete source", b"modified source archive"):
                with self.subTest(workflow=workflow, content=content):
                    result = self.run_check(
                        workflow, {"uqa-0.3.0.tar.gz": content}, self.manifest(files)
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("digest mismatch: uqa-0.3.0.tar.gz", result.stderr)

    def test_rejects_missing_and_unexpected_files(self):
        files = {"uqa-0.3.0.tar.gz": b"source archive"}
        for workflow in WORKFLOWS:
            for actual in ({}, {**files, "unexpected.whl": b"other package"}):
                with self.subTest(workflow=workflow, files=list(actual)):
                    result = self.run_check(workflow, actual, self.manifest(files))
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("inventory differs from verified files", result.stderr)

    def test_rejects_absent_preparation_output(self):
        for workflow in WORKFLOWS:
            with self.subTest(workflow=workflow):
                result = self.run_check(workflow, {}, "")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("inventory differs from verified files", result.stderr)


if __name__ == "__main__":
    unittest.main()
