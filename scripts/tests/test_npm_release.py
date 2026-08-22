#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import io
import json
import pathlib
import sys
import unittest
import urllib.error
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "npm-release.py"
SPEC = importlib.util.spec_from_file_location("uqa_npm_release", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
NPM_RELEASE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = NPM_RELEASE
SPEC.loader.exec_module(NPM_RELEASE)


class JSONResponse(io.StringIO):
    def __enter__(self) -> JSONResponse:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


class NPMReleaseRegistryTest(unittest.TestCase):
    def test_registry_package_reads_install_packument(self) -> None:
        package = {
            "name": "@cognica-io/uqa",
            "version": "0.1.6",
            "dist": {"integrity": "sha512-test", "shasum": "test"},
        }
        response = JSONResponse(json.dumps({"versions": {"0.1.6": package}}))
        with mock.patch.object(
            NPM_RELEASE.urllib.request, "urlopen", return_value=response
        ) as urlopen:
            actual = NPM_RELEASE.registry_package("@cognica-io/uqa", "0.1.6")

        self.assertEqual(actual, package)
        request = urlopen.call_args.args[0]
        self.assertEqual(
            request.full_url,
            "https://registry.npmjs.org/%40cognica-io%2Fuqa",
        )
        self.assertEqual(
            request.get_header("Accept"),
            "application/vnd.npm.install-v1+json",
        )

    def test_registry_package_waits_for_version_in_packument(self) -> None:
        response = JSONResponse(json.dumps({"versions": {}}))
        with mock.patch.object(
            NPM_RELEASE.urllib.request, "urlopen", return_value=response
        ):
            actual = NPM_RELEASE.registry_package("@cognica-io/uqa", "0.1.6")

        self.assertIsNone(actual)

    def test_registry_package_treats_not_found_as_pending(self) -> None:
        error = urllib.error.HTTPError(
            "https://registry.npmjs.org/%40cognica-io%2Fuqa",
            404,
            "Not Found",
            {},
            None,
        )
        with mock.patch.object(
            NPM_RELEASE.urllib.request, "urlopen", side_effect=error
        ):
            actual = NPM_RELEASE.registry_package("@cognica-io/uqa", "0.1.6")

        self.assertIsNone(actual)


if __name__ == "__main__":
    unittest.main()
