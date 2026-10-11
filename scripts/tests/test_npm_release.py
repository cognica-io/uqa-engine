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
import tarfile
import tempfile
import unittest
import urllib.error
from unittest import mock
from argparse import Namespace


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


class NPMReleaseArchiveTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        self.source = NPM_RELEASE.NODE_ROOT
        self.manifest = NPM_RELEASE.load_json(self.source / "package.json")
        self.version = self.manifest["version"]
        self.packed_manifest = dict(self.manifest)
        self.packed_manifest["optionalDependencies"] = {
            platform.package_name: self.version for platform in NPM_RELEASE.PLATFORMS
        }
        self.members = {
            relative: (self.source / relative).read_bytes()
            for relative in self.manifest["files"]
        }
        self.members["package.json"] = json.dumps(self.packed_manifest).encode()
        self.archive = self.directory / NPM_RELEASE.tarball_filename(
            NPM_RELEASE.ROOT_PACKAGE, self.version
        )

    def write_archive(self, path, members) -> None:
        with tarfile.open(path, "w:gz") as archive:
            for relative, payload in members.items():
                info = tarfile.TarInfo("package/" + relative)
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))

    def repair_args(self) -> Namespace:
        return Namespace(source_dir=self.source, archive=self.archive,
                         version=self.version, result_file=self.directory / "repair.json", npm="npm")

    def test_staging_includes_every_declared_file_and_exact_dependencies(self) -> None:
        stage = self.directory / "stage"
        stage.mkdir()
        NPM_RELEASE.stage_root_package(self.manifest, self.source, stage)
        actual = {str(path.relative_to(stage)): path.read_bytes()
                  for path in stage.rglob("*") if path.is_file()}
        self.assertEqual(set(actual), set(self.members))
        for relative in self.manifest["files"]:
            self.assertEqual(actual[relative], self.members[relative])
        self.assertEqual(json.loads(actual["package.json"]), self.packed_manifest)

    def test_staging_includes_a_future_helper_without_a_script_inventory_change(self) -> None:
        source = self.directory / "source"
        source.mkdir()
        (source / "new-helper.js").write_text('module.exports = "complete";\n')
        stage = self.directory / "stage"
        stage.mkdir()
        manifest = {"version": self.version, "files": ["new-helper.js"]}
        NPM_RELEASE.stage_root_package(manifest, source, stage)
        self.assertEqual((stage / "new-helper.js").read_bytes(),
                         (source / "new-helper.js").read_bytes())

    def test_archive_rejects_missing_notifications_types_and_diagnostics(self) -> None:
        for relative in ("notification-subscription.js", "notification-native.js",
                         "notifications.d.ts", "http-diagnostic.js"):
            with self.subTest(relative=relative):
                members = {name: payload for name, payload in self.members.items() if name != relative}
                with self.assertRaisesRegex(NPM_RELEASE.ReleaseError, relative.replace(".", r"\.")):
                    NPM_RELEASE.validate_root_package(
                        self.archive, self.packed_manifest, members, self.version
                    )

    def test_file_inventory_rejects_patterns_traversal_and_duplicates(self) -> None:
        for files in (["."], ["../outside.js"], ["/outside.js"], ["*.js"], ["a.js", "a.js"], [None]):
            with self.subTest(files=files), self.assertRaises(NPM_RELEASE.ReleaseError):
                NPM_RELEASE.declared_package_files({"files": files})

    def test_repair_preserves_published_archive_bytes(self) -> None:
        members = dict(self.members)
        del members["notification-subscription.js"]
        self.write_archive(self.archive, members)
        original = self.archive.read_bytes()
        with mock.patch.object(NPM_RELEASE, "registry_package", return_value={"version": self.version}), \
             mock.patch.object(NPM_RELEASE, "pack_stage") as pack:
            with self.assertRaisesRegex(NPM_RELEASE.ReleaseError, "immutable published"):
                NPM_RELEASE.repair_root_archive(self.repair_args())
        pack.assert_not_called()
        self.assertEqual(self.archive.read_bytes(), original)

    def test_repair_preserves_an_accepted_version_missing_from_install_metadata(self) -> None:
        members = dict(self.members)
        del members["notification-subscription.js"]
        self.write_archive(self.archive, members)
        original = self.archive.read_bytes()
        with mock.patch.object(NPM_RELEASE, "registry_package", return_value=None), \
             mock.patch.object(NPM_RELEASE, "registry_version_present", return_value=True), \
             mock.patch.object(NPM_RELEASE, "pack_stage") as pack:
            with self.assertRaisesRegex(NPM_RELEASE.ReleaseError, "immutable published"):
                NPM_RELEASE.repair_root_archive(self.repair_args())
        pack.assert_not_called()
        self.assertEqual(self.archive.read_bytes(), original)

    def test_repair_rejects_changed_tagged_content_before_registry_or_packaging(self) -> None:
        members = dict(self.members)
        del members["notification-subscription.js"]
        members["api.js"] = b"changed source"
        self.write_archive(self.archive, members)
        with mock.patch.object(NPM_RELEASE, "registry_package") as registry, \
             mock.patch.object(NPM_RELEASE, "pack_stage") as pack:
            with self.assertRaisesRegex(NPM_RELEASE.ReleaseError, "npm repair tagged file api.js"):
                NPM_RELEASE.repair_root_archive(self.repair_args())
        registry.assert_not_called()
        pack.assert_not_called()

    def test_unpublished_repair_restores_only_original_tagged_files(self) -> None:
        members = dict(self.members)
        omitted = [relative for relative in self.manifest["files"]
                   if relative.startswith("notification") or relative == "http-diagnostic.js"]
        for relative in omitted:
            del members[relative]
        self.write_archive(self.archive, members)

        def pack_fixture(npm, stage, output, cache, filename):
            payloads = {str(path.relative_to(stage)): path.read_bytes()
                        for path in stage.rglob("*") if path.is_file()}
            archive = output / filename
            self.write_archive(archive, payloads)
            return archive

        with mock.patch.object(NPM_RELEASE, "registry_package", return_value=None), \
             mock.patch.object(NPM_RELEASE, "registry_version_present", return_value=False), \
             mock.patch.object(NPM_RELEASE, "pack_stage", side_effect=pack_fixture):
            self.assertEqual(NPM_RELEASE.repair_root_archive(self.repair_args()), 0)
        manifest, actual = NPM_RELEASE.package_members(self.archive)
        self.assertEqual(manifest, self.packed_manifest)
        self.assertEqual(set(actual), set(self.members))
        for relative in self.manifest["files"]:
            self.assertEqual(actual[relative], self.members[relative])
        self.assertEqual(json.loads((self.directory / "repair.json").read_text()),
                         {"repaired": True, "version": self.version, "restored_files": sorted(omitted)})

    def test_complete_archive_is_not_repacked_or_queried(self) -> None:
        self.write_archive(self.archive, self.members)
        original = self.archive.read_bytes()
        with mock.patch.object(NPM_RELEASE, "registry_package") as registry, \
             mock.patch.object(NPM_RELEASE, "pack_stage") as pack:
            NPM_RELEASE.repair_root_archive(self.repair_args())
        registry.assert_not_called()
        pack.assert_not_called()
        self.assertEqual(self.archive.read_bytes(), original)


class NPMReleaseRegistryTest(unittest.TestCase):
    def test_exact_version_metadata_detects_an_accepted_root_package(self) -> None:
        package = {"name": "@cognica-io/uqa", "version": "0.5.3"}
        response = JSONResponse(json.dumps(package))
        with mock.patch.object(NPM_RELEASE.urllib.request, "urlopen", return_value=response) as urlopen:
            self.assertTrue(NPM_RELEASE.registry_version_present(package["name"], package["version"]))
        self.assertEqual(urlopen.call_args.args[0].full_url,
                         "https://registry.npmjs.org/%40cognica-io%2Fuqa/0.5.3")

    def test_exact_version_check_fails_closed_on_registry_errors(self) -> None:
        error = urllib.error.HTTPError("https://registry.npmjs.org/package/version", 503,
                                       "Unavailable", {}, None)
        with mock.patch.object(NPM_RELEASE.urllib.request, "urlopen", side_effect=error):
            with self.assertRaisesRegex(NPM_RELEASE.ReleaseError, "unpublished npm version"):
                NPM_RELEASE.registry_version_present("@cognica-io/uqa", "0.5.3")

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
