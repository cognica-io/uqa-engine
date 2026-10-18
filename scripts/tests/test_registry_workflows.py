#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the actual pre-publication integrity checks in both registry jobs."""

from __future__ import annotations

import hashlib
import json
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


class PythonPublicationResolutionTest(unittest.TestCase):
    def resolve(self, *, event="workflow_dispatch", version="0.0.1", tag="v0.0.1",
                choice="true", policy=None, status=0, release=None):
        source = (ROOT / ".github/workflows/pypi.yml").read_text()
        step = source.split("      - name: Resolve release and source publication policy\n", 1)[1]
        step = step.split("\n  prepare:\n", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / ".github").mkdir()
            (root / ".github/pypi-source-policy.json").write_text(json.dumps(policy or {}))
            (root / "pyproject.toml").write_text(f'[project]\nversion = "{version}"\n')
            binary = root / "bin"
            binary.mkdir()
            gh = binary / "gh"
            gh.write_text(textwrap.dedent("""\
                #!/usr/bin/env python3
                import json, os, sys
                response = os.environ['UQA_RELEASE_RESPONSE']
                status = int(os.environ['UQA_RELEASE_STATUS'])
                if sys.argv[1:] == ['api', 'repos/cognica-io/uqa-engine/releases/tags/' + os.environ['UQA_EXPECTED_TAG']]:
                    if not status:
                        matches = [release for page in json.loads(response) for release in page
                                   if release.get('tag_name') == os.environ['UQA_EXPECTED_TAG'] and not release.get('draft')]
                        response, status = (json.dumps(matches[0]), 0) if matches else ('{"status":"404"}', 1)
                else:
                    assert sys.argv[1:] == ['api', '--paginate', '--slurp', 'repos/cognica-io/uqa-engine/releases?per_page=100']
                print(response)
                raise SystemExit(status)
                """))
            gh.chmod(0o755)
            output = root / "output"
            result = subprocess.run(
                ["bash", "-e"], input=script, text=True, capture_output=True,
                cwd=root, check=False,
                env={**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                     "GITHUB_EVENT_NAME": event, "GITHUB_REPOSITORY": "cognica-io/uqa-engine",
                     "INPUT_TAG": tag, "INPUT_PUBLISH_SDIST": choice, "GITHUB_OUTPUT": str(output),
                     "UQA_EXPECTED_TAG": "v" + version, "UQA_RELEASE_STATUS": str(status),
                     "UQA_RELEASE_RESPONSE": json.dumps(release if release is not None else [[{"tag_name": "v" + version, "draft": False, "prerelease": False}]])},
            )
            values = dict(line.split("=", 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return result, values

    def test_source_policy_only_changes_the_exact_documented_filename(self):
        policy = {"uqa-0.0.1.tar.gz": "https://github.com/cognica-io/uqa-engine/actions/runs/123/job/456"}
        for tag, choice, expected in (("v0.0.1", "true", "false"),
                                      ("v0.0.2", "true", "true"),
                                      ("v0.0.2", "false", "false")):
            with self.subTest(tag=tag, choice=choice):
                result, values = self.resolve(tag=tag, choice=choice, policy=policy)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(values, {"ready": "true", "tag": tag, "publish_sdist": expected})

    def test_main_push_reconciles_the_current_public_release(self):
        for policy, expected in (({}, "true"), ({"uqa-0.0.1.tar.gz": "https://github.com/cognica-io/uqa-engine/actions/runs/123"}, "false")):
            with self.subTest(policy=policy):
                result, values = self.resolve(event="push", tag="", choice="", policy=policy)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(values, {"ready": "true", "tag": "v0.0.1", "publish_sdist": expected})

    def test_only_an_absent_release_can_skip_reconciliation(self):
        result, values = self.resolve(event="push", release=[[]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values, {"ready": "false"})
        for status in (401, 403, 404, 500):
            with self.subTest(status=status):
                result, values = self.resolve(event="push", status=1, release={"status": str(status)})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(values, {})

    def test_drafts_prereleases_and_incomplete_release_records_are_not_published(self):
        for release in ({"draft": True, "prerelease": False},
                        {"draft": False, "prerelease": True}, {}):
            with self.subTest(release=release):
                result, values = self.resolve(event="push", release=[[{"tag_name": "v0.0.1", **release}]])
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(values, {})

    def test_release_lookup_includes_later_pages_and_rejects_duplicate_tags(self):
        public = {"tag_name": "v0.0.1", "draft": False, "prerelease": False}
        other = {**public, "tag_name": "v0.0.2"}
        result, values = self.resolve(event="push", release=[[other], [public]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values["tag"], "v0.0.1")
        for pages in ([[other], [{**public, "draft": True}]], [[public], [public]]):
            with self.subTest(pages=pages):
                result, values = self.resolve(event="push", release=pages)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(values, {})

    def test_invalid_release_choices_and_rejection_evidence_fail_closed(self):
        for kwargs in ({"tag": "v0.0.1\ntag=v0.0.2"}, {"choice": "invalid"},
                       {"event": "pull_request"},
                       {"policy": {"uqa-0.0.1.tar.gz": "unverified"}},
                       {"policy": {"uqa-0.0.1.whl": "https://github.com/cognica-io/uqa-engine/actions/runs/123"}}):
            with self.subTest(kwargs=kwargs):
                result, values = self.resolve(**kwargs)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(values, {})


class PythonPublicationSelectionTest(unittest.TestCase):
    def test_explicit_source_location_preserves_selected_inventory_and_digests(self):
        source = (ROOT / ".github/workflows/pypi.yml").read_text()
        step = source.split("      - name: Select Python distributions for PyPI\n", 1)[1]
        step = step.split("\n      - ", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        original = {"uqa-0.3.0.tar.gz": b"source", "uqa.whl": b"wheel"}
        for choice in ("true", "false", "invalid"):
            with self.subTest(choice=choice), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                dist = root / "dist"
                dist.mkdir()
                for filename, content in original.items():
                    (dist / filename).write_bytes(content)
                output = root / "output"
                result = subprocess.run(
                    ["bash", "-e"], input=script, text=True, capture_output=True,
                    cwd=root, check=False,
                    env={**os.environ, "RELEASE_TAG": "v0.3.0",
                         "PUBLISH_SDIST": choice, "GITHUB_OUTPUT": str(output)},
                )
                if choice == "invalid":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())
                    continue
                self.assertEqual(result.returncode, 0, result.stderr)
                expected = original if choice == "true" else {"uqa.whl": b"wheel"}
                self.assertEqual({p.name: p.read_bytes() for p in dist.iterdir()}, expected)
                self.assertEqual(
                    set(output.read_text().splitlines()),
                    {"digests<<UQA_RELEASE_DIGESTS", "UQA_RELEASE_DIGESTS",
                     *RegistryTransferIntegrityTest.manifest(expected).splitlines()},
                )


class PythonRegistryInventoryTest(unittest.TestCase):
    def test_reconciliation_requires_the_exact_existing_file_hashes(self):
        source = (ROOT / ".github/workflows/pypi.yml").read_text()
        step = source.split("      - name: Check whether PyPI already has this exact release\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n      - ", 1)[0])
        files = {"first.whl": b"first verified wheel", "second.whl": b"second verified wheel"}
        exact = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
        cases = ((exact, True), ({"first.whl": exact["first.whl"]}, False),
                 ({**exact, "unexpected.whl": "f" * 64}, False),
                 ({**exact, "first.whl": "0" * 64}, False))
        for remote, valid in cases:
            with self.subTest(remote=remote), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                (root / "dist").mkdir()
                for name, data in files.items():
                    (root / "dist" / name).write_bytes(data)
                (root / "sitecustomize.py").write_text(textwrap.dedent("""\
                    import io, os, urllib.request
                    urllib.request.urlopen = lambda request: io.BytesIO(os.environ['UQA_PYPI_RESPONSE'].encode())
                    """))
                output = root / "output"
                response = {"urls": [{"filename": name, "digests": {"sha256": digest}}
                                     for name, digest in remote.items()]}
                result = subprocess.run(
                    ["bash", "-e"], input=script, text=True, capture_output=True,
                    cwd=root, check=False,
                    env={**os.environ, "PYTHONPATH": str(root), "RELEASE_TAG": "v0.0.1",
                         "GITHUB_OUTPUT": str(output), "UQA_PYPI_RESPONSE": json.dumps(response)},
                )
                if valid:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output.read_text(), "publish=false\n")
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("different release inventory", result.stderr)
                    self.assertFalse(output.exists())


class NpmPublicationSelectionTest(unittest.TestCase):
    def test_downloads_only_npm_packages_from_a_mixed_release(self):
        source = (ROOT / ".github/workflows/npmjs.yml").read_text()
        step = source.split("      - name: Download the released npm distributions\n", 1)[1]
        step = step.split("\n      - ", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        version = "0.0.0"
        npm = {
            f"cognica-io-uqa{suffix}-{version}.tgz"
            for suffix in ("", "-wasm", "-linux-x64-gnu", "-linux-arm64-gnu",
                           "-darwin-x64", "-darwin-arm64", "-win32-x64-msvc",
                           "-win32-arm64-msvc")
        }
        other = {
            f"uqa-cli-{version}-{arch}-{target}.tgz"
            for arch in ("x86_64", "aarch64")
            for target in ("unknown-linux-gnu", "apple-darwin")
        } | {f"uqa-cli-{version}-x86_64-pc-windows-msvc.zip",
             f"uqa-{version}.tar.gz", "uqa.whl", "install-usql.sh"}
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            assets = root / "assets"
            assets.mkdir()
            for name in npm | other:
                (assets / name).write_bytes(name.encode())
            binary = root / "bin"
            binary.mkdir()
            gh = binary / "gh"
            gh.write_text(textwrap.dedent("""\
                #!/usr/bin/env python3
                import argparse, os, pathlib, shutil
                parser = argparse.ArgumentParser()
                parser.add_argument('command', choices=['release'])
                parser.add_argument('action', choices=['download'])
                parser.add_argument('tag')
                parser.add_argument('--dir', required=True)
                parser.add_argument('--pattern', action='append', required=True)
                args = parser.parse_args()
                assert args.tag == os.environ['RELEASE_TAG']
                for pattern in args.pattern:
                    for asset in pathlib.Path(os.environ['UQA_RELEASE_ASSETS']).glob(pattern):
                        shutil.copyfile(asset, pathlib.Path(args.dir) / asset.name)
                """))
            gh.chmod(0o755)
            result = subprocess.run(
                ["bash", "-e"], input=script, text=True, capture_output=True,
                cwd=root, check=False,
                env={**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                     "RELEASE_TAG": f"v{version}", "UQA_RELEASE_ASSETS": str(assets)},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                {path.name: path.read_bytes() for path in (root / "dist").iterdir()},
                {name: name.encode() for name in npm},
            )


if __name__ == "__main__":
    unittest.main()
