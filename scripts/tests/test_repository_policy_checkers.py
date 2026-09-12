#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import ast
import importlib.util
import hashlib
import io
import json
import pathlib
import sys
import tempfile
import tarfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_script(module_name: str, filename: str) -> object:
    spec = importlib.util.spec_from_file_location(module_name, ROOT / "scripts" / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


INTEGRATION = load_script(
    "uqa_check_integration_test_harnesses", "check-integration-test-harnesses.py"
)
LICENSES = load_script("uqa_check_release_licenses", "check-release-licenses.py")


class RepositoryPolicyCheckerTest(unittest.TestCase):
    def test_nori_resources_require_every_notice_and_exact_artifact_hashes(self) -> None:
        payloads = {name: name.encode() for name in LICENSES.NORI_FILES}
        manifest = {
            "format": "uqa-nori-bundled-resource", "format_version": 1,
            "files": [{"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                      for name, data in payloads.items()],
        }
        LICENSES.check_nori_payloads(json.dumps(manifest).encode(), payloads.__getitem__)
        for name in LICENSES.NORI_FILES:
            damaged = dict(payloads)
            damaged[name] = b"changed"
            with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, "size or hash differs"):
                LICENSES.check_nori_payloads(json.dumps(manifest).encode(), damaged.__getitem__)
        manifest["files"].pop()
        with self.assertRaisesRegex(RuntimeError, "incomplete or duplicate"):
            LICENSES.check_nori_payloads(json.dumps(manifest).encode(), payloads.__getitem__)

    def test_nori_archive_rejects_missing_and_changed_attribution_files(self) -> None:
        payloads = {name: name.encode() for name in LICENSES.NORI_FILES}
        manifest = json.dumps({
            "format": "uqa-nori-bundled-resource", "format_version": 1,
            "files": [{"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                      for name, data in payloads.items()],
        }).encode()
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            path = root / "crates/uqa-nori-data/data/resource_manifest.json"
            path.parent.mkdir(parents=True)
            path.write_bytes(manifest)
            archive_path = root / "uqa-nori-data-0.0.0.crate"
            for mode in ("valid", "missing", "changed"):
                files = {**payloads, "data/resource_manifest.json": manifest,
                         "LICENSE": b"license", "LICENSE-NOTICE.md": b"notice"}
                if mode == "missing":
                    del files["THIRD-PARTY/LUCENE-NOTICE.txt"]
                elif mode == "changed":
                    files["THIRD-PARTY/LUCENE-NOTICE.txt"] = b"changed"
                with tarfile.open(archive_path, "w:gz") as archive:
                    for name, data in files.items():
                        info = tarfile.TarInfo(f"uqa-nori-data-0.0.0/{name}")
                        info.size = len(data)
                        archive.addfile(info, io.BytesIO(data))
                with mock.patch.object(LICENSES, "ROOT", root):
                    if mode == "valid":
                        LICENSES.check_archive(archive_path, {"LICENSE": b"license"})
                    else:
                        with self.subTest(mode=mode), self.assertRaises(RuntimeError):
                            LICENSES.check_archive(archive_path, {"LICENSE": b"license"})

    def test_checkers_import_without_tomllib(self) -> None:
        with mock.patch.dict(sys.modules, {"tomllib": None}):
            load_script(
                "uqa_check_integration_without_tomllib",
                "check-integration-test-harnesses.py",
            )
            load_script(
                "uqa_check_licenses_without_tomllib",
                "check-release-licenses.py",
            )

    def test_checkers_accept_python_3_8_grammar(self) -> None:
        for filename in (
            "check-integration-test-harnesses.py",
            "check-release-licenses.py",
        ):
            with self.subTest(filename=filename):
                source = (ROOT / "scripts" / filename).read_text(encoding="utf-8")
                ast.parse(source, filename=filename, feature_version=8)

    def test_integration_targets_come_from_cargo_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            crate = pathlib.Path(temporary) / "example"
            tests = crate / "tests"
            tests.mkdir(parents=True)
            root = tests / "integration.rs"
            child = tests / "queries.rs"
            root.write_text('#[path = "queries.rs"]\nmod queries;\n', encoding="utf-8")
            child.write_text("#[test]\nfn query() {}\n", encoding="utf-8")
            package = {
                "name": "example",
                "manifest_path": str(crate / "Cargo.toml"),
                "targets": [
                    {
                        "kind": ["test"],
                        "name": "integration",
                        "src_path": str(root),
                    }
                ],
            }

            self.assertEqual(
                INTEGRATION.verify_crate(package),
                ("example", {"integration"}, 1, 2),
            )

    def test_integration_checker_rejects_multiple_metadata_targets(self) -> None:
        package = {
            "name": "example",
            "manifest_path": "/tmp/example/Cargo.toml",
            "targets": [
                {"kind": ["test"], "name": "first", "src_path": "/tmp/first.rs"},
                {"kind": ["test"], "name": "second", "src_path": "/tmp/second.rs"},
            ],
        }

        with self.assertRaisesRegex(RuntimeError, "exactly one integration test target"):
            INTEGRATION.verify_crate(package)

    def test_release_checker_reads_required_project_license_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "pyproject.toml").write_text(
                """
[project]
name = "uqa"
license = "AGPL-3.0-only"
license-files = [
    "LICENSE",
    "LICENSING.md",
    "LICENSES/*.txt",
]

[project.urls]
Repository = "https://example.test/uqa"
""".lstrip(),
                encoding="utf-8",
            )
            with mock.patch.object(LICENSES, "ROOT", root):
                LICENSES.check_maturin_sources()

    def test_release_checker_rejects_missing_license_file_pattern(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "pyproject.toml").write_text(
                """
[project]
license = "AGPL-3.0-only"
license-files = ["LICENSE", "LICENSING.md"]
""".lstrip(),
                encoding="utf-8",
            )
            with (
                mock.patch.object(LICENSES, "ROOT", root),
                self.assertRaisesRegex(RuntimeError, "license-files must include"),
            ):
                LICENSES.check_maturin_sources()

    def test_release_checker_ignores_declarations_inside_multiline_values(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "pyproject.toml").write_text(
                '''
[project]
description = """
license = "AGPL-3.0-only"
license-files = ["LICENSE", "LICENSING.md", "LICENSES/*.txt"]
"""
name = "uqa"
'''.lstrip(),
                encoding="utf-8",
            )
            with (
                mock.patch.object(LICENSES, "ROOT", root),
                self.assertRaisesRegex(RuntimeError, "must declare project.license"),
            ):
                LICENSES.check_maturin_sources()


if __name__ == "__main__":
    unittest.main()
