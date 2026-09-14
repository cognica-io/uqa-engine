#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import hashlib
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest
from unittest import mock
from zipfile import ZipFile


ROOT = pathlib.Path(__file__).resolve().parents[2] / "tests" / "parity" / "nori"
sys.path.insert(0, str(ROOT))
try:
    spec = importlib.util.spec_from_file_location("nori_export_model", ROOT / "export_model.py")
    export = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(export)
finally:
    sys.path.pop(0)
runtime = sys.modules["reference_runtime"]


class NoriModelExportTest(unittest.TestCase):
    def test_corrupted_cache_is_rejected_without_download_or_replacement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            jar = root / "lucene-core-test.jar"
            jar.write_bytes(b"changed")
            manifest = {
                "lucene_version": "test",
                "jars": [{
                    "artifact": "lucene-core", "url": "https://example.invalid/jar",
                    "bytes": 7, "sha256": hashlib.sha256(b"correct").hexdigest(),
                }],
            }
            (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with mock.patch.object(runtime, "ROOT", root), mock.patch.object(runtime, "urlopen") as download:
                with self.assertRaisesRegex(RuntimeError, "Cached artifact checksum mismatch"):
                    runtime.prepare_jars(root, offline=False)
                download.assert_not_called()
            self.assertEqual(jar.read_bytes(), b"changed")

    def test_unknown_or_changed_dictionary_resources_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            resource = "org/apache/lucene/analysis/ko/dict/CharacterDefinition.dat"
            manifest = {
                "lucene_version": "test",
                "dictionary_resources": [{
                    "path": resource, "bytes": 3,
                    "sha256": hashlib.sha256(b"abc").hexdigest(),
                }],
            }
            jar = root / "lucene-analysis-nori-test.jar"
            with ZipFile(jar, "w") as archive:
                archive.writestr(resource, b"abd")
            with self.assertRaisesRegex(RuntimeError, "Dictionary resource checksum mismatch"):
                runtime.verify_dictionary_resources(manifest, root)
            with ZipFile(jar, "a") as archive:
                archive.writestr("org/apache/lucene/analysis/ko/dict/Extra.dat", b"x")
            with self.assertRaisesRegex(RuntimeError, "resource inventory"):
                runtime.verify_dictionary_resources(manifest, root)

    def test_same_size_model_corruption_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name in export.FILES:
                (root / name).write_bytes(b"abcd")
            expected = {"files": export.file_inventory(root)}
            (root / "model_manifest.json").write_text(export.manifest_text(expected), encoding="utf-8")
            export.check_files(root, expected)
            (root / "unicode.bin").write_bytes(b"abce")
            with self.assertRaisesRegex(RuntimeError, "Exported model checksum mismatch"):
                export.check_files(root, expected)

    def test_manifest_cannot_refer_to_unexpected_files(self):
        expected = {"files": [{"path": "../outside.bin", "bytes": 0, "sha256": ""}]}
        with self.assertRaisesRegex(RuntimeError, "file inventory"):
            export.check_files(pathlib.Path("/not-read"), expected)

    def test_export_refuses_to_overwrite_existing_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            marker = root / "existing"
            marker.write_text("retain", encoding="utf-8")
            with mock.patch.object(sys, "argv", ["export_model.py", "--output", temporary, "--write-manifest"]):
                with mock.patch.object(export, "prepare_jars") as prepare:
                    with self.assertRaisesRegex(RuntimeError, "Output already exists"):
                        export.main()
                    prepare.assert_not_called()
            self.assertEqual(marker.read_text(encoding="utf-8"), "retain")

    def test_changed_model_vocabulary_requires_manifest_review(self):
        with self.assertRaisesRegex(RuntimeError, "reviewed manifest"):
            export.compare_manifest({"model": {"pos_tags": ["NNG"]}}, {"model": {"pos_tags": ["NNP"]}})


if __name__ == "__main__":
    unittest.main()
