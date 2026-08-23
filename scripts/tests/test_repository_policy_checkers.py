#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import ast
import importlib.util
import pathlib
import sys
import tempfile
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


if __name__ == "__main__":
    unittest.main()
