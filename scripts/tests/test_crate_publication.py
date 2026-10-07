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
import shutil
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
    def run_publisher(self, registry_statuses: list[int], publish_status: int = 0,
                      package_status: int = 0, license_status: int = 0,
                      index_failures: int = 0, separate_source: bool = False) -> tuple[subprocess.CompletedProcess, str]:
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            source = directory / "release-source" if separate_source else directory
            source.mkdir(exist_ok=True)
            (directory / "scripts").mkdir()
            for name in ("publish-crates.sh", "package-publishable-crates.sh"):
                shutil.copyfile(ROOT / "scripts" / name, directory / "scripts" / name)
            (source / "Cargo.toml").write_text('[workspace.package]\nversion = "0.5.1"\n')
            checker = directory / "python3"
            checker.write_text(f"#!{sys.executable}\n" + """import json, os, pathlib, sys
root = pathlib.Path(os.environ['UQA_TEST_COMMANDS'])
if sys.argv[1] == '-c':
    data = json.load(sys.stdin)
    for package in data['packages']:
        print(package['name'] + '\\t' + package['version'])
    sys.exit(0)
if sys.argv[1] == 'scripts/check-release-licenses.py':
    assert (root / 'archives-built').exists()
    status = int(os.environ['UQA_TEST_LICENSE'])
    if status == 0:
        (root / 'archives-verified').touch()
    sys.exit(status)
assert (root / 'archives-verified').exists(), 'registry access before all archives build and pass validation'
assert pathlib.Path(sys.argv[1]).name == 'check-published-crate.py'
if '--wait-index' in sys.argv:
    sys.exit(0)
if '--dependency-error-log' in sys.argv:
    sys.exit(0 if int(os.environ['UQA_TEST_INDEX_FAILURES']) else 1)
count = root / 'probes'
attempt = int(count.read_text()) if count.exists() else 0
count.write_text(str(attempt + 1))
assert sys.argv[2] == 'uqa'
sys.exit(json.loads(os.environ['UQA_TEST_REGISTRY'])[attempt])
""")
            cargo = directory / "cargo"
            cargo.write_text(f"#!{sys.executable}\n" + """import json, os, pathlib, sys
root = pathlib.Path(os.environ['UQA_TEST_COMMANDS'])
if sys.argv[1] == 'metadata':
    packages = [{'id': name, 'name': name, 'version': '0.5.1'} for name in ('uqa-core', 'uqa')]
    print(json.dumps({'packages': packages, 'workspace_members': ['uqa-core', 'uqa']}))
    sys.exit(0)
if sys.argv[1] == 'package':
    assert '--no-verify' not in sys.argv, 'archive compilation must not be skipped'
    assert sys.argv[2:] == ['--locked', '-p', 'uqa-core', '-p', 'uqa']
    status = int(os.environ['UQA_TEST_PACKAGE'])
    if status == 0:
        (root / 'target/package').mkdir(parents=True)
        for name in ('uqa-core', 'uqa'):
            (root / f'target/package/{name}-0.5.1.crate').touch()
        (root / 'archives-built').touch()
    sys.exit(status)
assert sys.argv[1] == 'publish'
assert (root / 'archives-verified').exists(), 'upload before every archive is verified'
(root / 'cargo-args').write_text(' '.join(sys.argv[1:]))
attempts = root / 'publish-attempts'
attempt = int(attempts.read_text()) if attempts.exists() else 0
attempts.write_text(str(attempt + 1))
if attempt < int(os.environ['UQA_TEST_INDEX_FAILURES']):
    print('failed to select a version for the requirement `uqa-engine = "^0.5.1"`')
    print('location searched: crates.io index')
    sys.exit(101)
sys.exit(int(os.environ['UQA_TEST_PUBLISH']))
""")
            sleep = directory / "sleep"
            sleep.write_text("#!/bin/sh\nexit 0\n")
            sleep.chmod(0o755)
            checker.chmod(0o755)
            cargo.chmod(0o755)
            result = subprocess.run(
                ["bash", "scripts/publish-crates.sh", "--live", "--start-at", "uqa"],
                cwd=directory, text=True, capture_output=True,
                env={**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"],
                     "UQA_RELEASE_SOURCE_ROOT": str(source),
                     "UQA_TEST_COMMANDS": str(source), "UQA_TEST_REGISTRY": json.dumps(registry_statuses),
                     "UQA_TEST_PUBLISH": str(publish_status), "UQA_TEST_PACKAGE": str(package_status),
                     "UQA_TEST_LICENSE": str(license_status), "UQA_TEST_INDEX_FAILURES": str(index_failures)},
            )
            calls = source / "cargo-args"
            return result, calls.read_text() if calls.exists() else ""

    def test_any_archive_build_failure_prevents_the_first_upload(self) -> None:
        result, calls = self.run_publisher([], package_status=31)
        self.assertEqual(result.returncode, 31, result.stderr)
        self.assertEqual(calls, "")

    def test_archive_license_failure_prevents_the_first_upload(self) -> None:
        result, calls = self.run_publisher([], license_status=32)
        self.assertEqual(result.returncode, 32, result.stderr)
        self.assertEqual(calls, "")

    def test_existing_version_skips_cargo_publication(self) -> None:
        result, calls = self.run_publisher([0])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "")

    def test_missing_version_reaches_cargo_publication(self) -> None:
        result, calls = self.run_publisher([1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "publish -p uqa --locked")

    def test_recovery_builds_the_separate_original_source_tree(self) -> None:
        result, calls = self.run_publisher([1], separate_source=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "publish -p uqa --locked")

    def test_dependency_index_propagation_retries_only_after_all_archives_build(self) -> None:
        result, calls = self.run_publisher([1], index_failures=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, "publish -p uqa --locked")
        self.assertEqual(result.stderr.count("after dependency index propagation"), 2)

    def test_dependency_index_retries_have_a_fixed_bound(self) -> None:
        result, _ = self.run_publisher([1, 1], index_failures=7)
        self.assertEqual(result.returncode, 101, result.stderr)
        self.assertEqual(result.stderr.count("after dependency index propagation"), 6)

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

    def test_index_requires_the_exact_visible_non_yanked_version(self) -> None:
        old = {"name": "uqa-engine", "vers": "0.5.0", "yanked": False}
        current = {**old, "vers": "0.5.1"}
        for entries, expected in (([old], False), ([old, current], True)):
            data = b"\n".join(json.dumps(entry).encode() for entry in entries)
            with mock.patch.object(CHECKER.urllib.request, "urlopen", return_value=io.BytesIO(data)) as request:
                self.assertEqual(CHECKER.indexed("uqa-engine", "0.5.1"), expected)
                self.assertEqual(request.call_args.args[0].full_url, "https://index.crates.io/uq/a-/uqa-engine")
        for entry in ({**current, "yanked": True}, {**current, "name": "other"}, {}):
            with mock.patch.object(CHECKER.urllib.request, "urlopen", return_value=io.BytesIO(json.dumps(entry).encode())):
                with self.assertRaises(RuntimeError):
                    CHECKER.indexed("uqa-engine", "0.5.1")

    def test_index_wait_stops_when_visible_and_times_out_when_missing(self) -> None:
        with mock.patch.object(CHECKER, "indexed", side_effect=[False, True]), mock.patch.object(CHECKER.time, "sleep") as sleep:
            CHECKER.wait_indexed("uqa-engine", "0.5.1")
        sleep.assert_called_once()
        with mock.patch.object(CHECKER, "indexed", return_value=False), mock.patch.object(CHECKER.time, "monotonic", side_effect=[0, 20]), mock.patch.object(CHECKER.time, "sleep") as sleep:
            with self.assertRaisesRegex(RuntimeError, "did not expose"):
                CHECKER.wait_indexed("uqa-engine", "0.5.1", timeout=10)
        sleep.assert_not_called()

    def test_only_exact_uqa_dependency_resolution_is_retryable(self) -> None:
        log = 'failed to select a version for the requirement `uqa-engine = "^0.5.1"`\nlocation searched: crates.io index'
        self.assertEqual(CHECKER.missing_dependency(log, "0.5.1"), "uqa-engine")
        for rejected in (log.replace("0.5.1", "0.5.0"), log.replace("uqa-engine", "third-party"), log.replace("crates.io index", "another registry"), "error[E0308]: mismatched types"):
            self.assertIsNone(CHECKER.missing_dependency(rejected, "0.5.1"))

    def test_dependency_retry_requires_confirmed_publication_and_index_visibility(self) -> None:
        arguments = ["check", "uqa-api", "0.5.1", "--dependency-error-log", "error.log"]
        log = 'failed to select a version for the requirement `uqa-engine = "^0.5.1"`\nlocation searched: crates.io index'
        for available in (False, True):
            with mock.patch.object(sys, "argv", arguments), \
                 mock.patch.object(CHECKER.pathlib.Path, "read_text", return_value=log), \
                 mock.patch.object(CHECKER, "published", return_value=available) as published, \
                 mock.patch.object(CHECKER, "wait_indexed") as wait:
                self.assertEqual(CHECKER.main(), 0 if available else 1)
                published.assert_called_once_with("uqa-engine", "0.5.1")
                if available:
                    wait.assert_called_once_with("uqa-engine", "0.5.1")
                else:
                    wait.assert_not_called()


class ReleaseAssetRetentionTest(unittest.TestCase):
    def test_registry_publication_waits_for_all_package_checks(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        for name, required in (
            ("crates-io", {"resolve", "python", "javascript", "benchmarks"}),
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
        workflow = (ROOT / ".github/workflows/release-assets.yml").read_text()

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
