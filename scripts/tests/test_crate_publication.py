#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import io
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
import urllib.error
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("published_crate", ROOT / "scripts/check-published-crate.py")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class CratePublicationTest(unittest.TestCase):
    def run_publisher(self, registry_statuses: list[int], publish_status: int = 0) -> tuple[subprocess.CompletedProcess, str]:
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            checker = directory / "python3"
            checker.write_text(f"#!{sys.executable}\n" + """import json, os, pathlib, sys
root = pathlib.Path(os.environ['UQA_TEST_COMMANDS'])
count = root / 'probes'
attempt = int(count.read_text()) if count.exists() else 0
count.write_text(str(attempt + 1))
assert sys.argv[1] == 'scripts/check-published-crate.py'
assert sys.argv[2] == 'uqa'
sys.exit(json.loads(os.environ['UQA_TEST_REGISTRY'])[attempt])
""")
            cargo = directory / "cargo"
            cargo.write_text(f"#!{sys.executable}\n" + """import os, pathlib, sys
root = pathlib.Path(os.environ['UQA_TEST_COMMANDS'])
(root / 'cargo-args').write_text(' '.join(sys.argv[1:]))
sys.exit(int(os.environ['UQA_TEST_PUBLISH']))
""")
            checker.chmod(0o755)
            cargo.chmod(0o755)
            result = subprocess.run(
                ["bash", "scripts/publish-crates.sh", "--live", "--start-at", "uqa"],
                cwd=ROOT, text=True, capture_output=True,
                env={**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"],
                     "UQA_TEST_COMMANDS": temporary, "UQA_TEST_REGISTRY": json.dumps(registry_statuses),
                     "UQA_TEST_PUBLISH": str(publish_status)},
            )
            calls = directory / "cargo-args"
            return result, calls.read_text() if calls.exists() else ""

    def test_existing_version_skips_cargo_publication(self) -> None:
        result, calls = self.run_publisher([0])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "")

    def test_missing_version_reaches_cargo_publication(self) -> None:
        result, calls = self.run_publisher([1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "publish -p uqa --locked")

    def test_registry_failure_does_not_start_publication(self) -> None:
        result, calls = self.run_publisher([2])
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(calls, "")

    def test_publish_failure_requires_registry_confirmation(self) -> None:
        for followup in (0, 1, 2):
            with self.subTest(followup=followup):
                result, calls = self.run_publisher([1, followup], publish_status=7)
                self.assertEqual(result.returncode, 0 if followup == 0 else 7, result.stderr)
                self.assertEqual(calls, "publish -p uqa --locked")

    def test_exact_available_version_is_already_published(self) -> None:
        response = io.BytesIO(json.dumps({"version": {"crate": "uqa-core", "num": "0.4.5", "yanked": False}}).encode())
        with mock.patch.object(CHECKER.urllib.request, "urlopen", return_value=response) as open_url:
            self.assertTrue(CHECKER.published("uqa-core", "0.4.5"))
        request = open_url.call_args.args[0]
        self.assertEqual(request.full_url, "https://crates.io/api/v1/crates/uqa-core/0.4.5")

    def test_only_not_found_allows_a_new_publication(self) -> None:
        for status in (404, 403, 429, 500):
            error = urllib.error.HTTPError("registry", status, "response", {}, None)
            with self.subTest(status=status), mock.patch.object(CHECKER.urllib.request, "urlopen", side_effect=error):
                if status == 404:
                    self.assertFalse(CHECKER.published("uqa-core", "0.4.5"))
                else:
                    with self.assertRaises(RuntimeError):
                        CHECKER.published("uqa-core", "0.4.5")

    def test_network_and_invalid_json_fail_closed(self) -> None:
        with mock.patch.object(CHECKER.urllib.request, "urlopen", side_effect=urllib.error.URLError("offline")):
            with self.assertRaises(RuntimeError):
                CHECKER.published("uqa-core", "0.4.5")
        with mock.patch.object(CHECKER.urllib.request, "urlopen", return_value=io.BytesIO(b"invalid")):
            with self.assertRaises(RuntimeError):
                CHECKER.published("uqa-core", "0.4.5")

    def test_other_missing_or_yanked_versions_cannot_count_as_published(self) -> None:
        valid = {"crate": "uqa-core", "num": "0.4.5", "yanked": False}
        for body in ([], {}, {"version": None},
                     {"version": {**valid, "crate": "different"}},
                     {"version": {**valid, "num": "0.4.0"}},
                     {"version": {**valid, "yanked": True}},
                     {"version": {**valid, "yanked": 0}},
                     {"version": {key: value for key, value in valid.items() if key != "yanked"}}):
            with self.subTest(body=body), mock.patch.object(CHECKER.urllib.request, "urlopen", return_value=io.BytesIO(json.dumps(body).encode())):
                with self.assertRaises(RuntimeError):
                    CHECKER.published("uqa-core", "0.4.5")


class ReleaseAssetRetentionTest(unittest.TestCase):
    def test_registry_publication_waits_for_all_package_checks(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        for name, required in (
            ("crates-io", {"resolve", "python", "javascript"}),
            ("release", {"resolve", "python", "javascript", "crates-io"}),
        ):
            with self.subTest(job=name):
                job = workflow.split(f"\n  {name}:\n", 1)[1]
                job = re.split(r"\n  [a-z][a-z-]*:\n", job, maxsplit=1)[0]
                dependencies = re.search(r"^    needs: \[([^]]+)\]$", job, re.M)
                self.assertIsNotNone(dependencies)
                self.assertTrue(required.issubset(set(dependencies.group(1).split(", "))))
                self.assertIsNone(re.search(r"^    if:", job, re.M),
                                  "Publication must retain GitHub's default successful-needs gate")

    def test_completion_keeps_published_bytes_and_uploads_only_missing_assets(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text()

        def command(name: str) -> str:
            step = workflow.split(f"      - name: {name}\n", 1)[1].split("\n      - ", 1)[0]
            return textwrap.dedent(step.split("        run: |\n", 1)[1])

        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            (directory / "dist").mkdir()
            (directory / "dist/existing.tgz").write_bytes(b"rebuilt bytes")
            (directory / "dist/missing.tar.gz").write_bytes(b"new source distribution")
            (directory / "release-notes.md").write_text("Release notes")
            gh = directory / "gh"
            gh.write_text(f"#!{sys.executable}\n" + """import json, pathlib, sys
assert sys.argv[1] == 'release'
operation = sys.argv[2]
if operation == 'view':
    print('existing.tgz')
elif operation == 'download':
    assert sys.argv[sys.argv.index('--pattern') + 1] == 'existing.tgz'
    pathlib.Path('dist/existing.tgz').write_bytes(b'original published bytes')
elif operation == 'upload':
    assert '--clobber' not in sys.argv
    assert sys.argv[4:] == ['dist/missing.tar.gz']
    pathlib.Path('uploaded').write_text(sys.argv[4])
elif operation != 'edit':
    raise AssertionError(operation)
""")
            gh.chmod(0o755)
            env = {**os.environ, "PATH": temporary + os.pathsep + os.environ["PATH"], "TAG": "v0.4.5"}
            for name in ("Retain existing release assets", "Publish the release"):
                result = subprocess.run(["bash", "-c", command(name)], cwd=directory, env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((directory / "dist/existing.tgz").read_bytes(), b"original published bytes")
            self.assertEqual((directory / "uploaded").read_text(), "dist/missing.tar.gz")
