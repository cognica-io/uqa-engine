#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from zipfile import ZipFile


ROOT = Path(__file__).resolve().parents[2] / "tests/parity/kuromoji"
sys.path.insert(0, str(ROOT))
try:
    def load(name, filename):
        spec = importlib.util.spec_from_file_location(name, ROOT / filename)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    regenerate = load("kuromoji_csv_regeneration", "regenerate_dictionary.py")
    export = load("kuromoji_model_export", "export_model.py")
finally:
    sys.path.pop(0)
runtime = export.runtime


def digest(data):
    return hashlib.sha256(data).hexdigest()


def patch_bytes(target="Noun.proper.csv", old="昭和", new="令和"):
    return f"--- {target}\n+++ {target}\n@@ -1 +1,2 @@\n {old}\n+{new}\n".encode("euc-jp")


class KuromojiReferenceTest(unittest.TestCase):
    def test_reviewed_source_patch_resources_and_model_are_complete(self):
        reference = json.loads((ROOT / "manifest.json").read_text())
        source = json.loads((ROOT / "csv_manifest.json").read_text())
        model = json.loads((ROOT / "model_manifest.json").read_text())
        self.assertEqual(source["reference_manifest_sha256"], runtime.sha256(ROOT / "manifest.json"))
        self.assertEqual(source["builder_source_sha256"], runtime.sha256(ROOT / regenerate.ENTRYPOINT))
        self.assertEqual(model["exporter_sha256"], runtime.sha256(ROOT / "KuromojiModel.java"))
        self.assertEqual(model["reference"], {name: reference[name] for name in export.EXPORTER.reference_fields})
        self.assertEqual(source["resources"], sorted(reference["dictionary_resources"], key=lambda row: row["path"]))
        self.assertEqual(len(source["resources"]), 9)
        original = {row["path"]: row for row in source["original_inputs"]}
        patched = {row["path"]: row for row in source["patched_inputs"]}
        self.assertEqual(len(original), 29)
        self.assertEqual(original.keys(), patched.keys())
        self.assertEqual([name for name in original if original[name] != patched[name]], ["Noun.proper.csv"])
        self.assertEqual(source["patch"], reference["dictionary_patch"])
        self.assertEqual(source["builder"]["encoding"], "euc-jp")
        self.assertFalse(source["builder"]["normalize_entries"])
        self.assertEqual(model["model"]["runtime"], reference["runtime"])
        self.assertEqual(model["model"]["surface_count"], 325872)
        self.assertEqual(model["model"]["word_count"], 392127)
        self.assertEqual(model["model"]["unknown_word_count"], 41)
        self.assertEqual(model["model"]["stop_word_count"], 109)
        self.assertEqual(model["model"]["stop_tag_count"], 27)
        self.assertEqual(model["model"]["completion_mapping_count"], 329)
        self.assertEqual([row["path"] for row in model["files"]], list(export.FILES))

    def test_applies_euc_jp_patch_without_transcoding(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inputs = root / "input"
            inputs.mkdir()
            target = inputs / "Noun.proper.csv"
            target.write_bytes("昭和\n".encode("euc-jp"))
            patch = root / "source.patch"
            patch.write_bytes(patch_bytes())
            regenerate.apply_patch(inputs, patch)
            self.assertEqual(target.read_bytes(), "昭和\n令和\n".encode("euc-jp"))

    def test_patch_cannot_change_other_files_or_apply_to_different_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inputs = root / "input"
            inputs.mkdir()
            target = inputs / "Noun.proper.csv"
            original = "昭和\n".encode("euc-jp")
            target.write_bytes(original)
            patch = root / "source.patch"
            for data in [patch_bytes(target="extra.csv"), patch_bytes() + patch_bytes(target="extra.csv")]:
                patch.write_bytes(data)
                with self.assertRaisesRegex(RuntimeError, "unexpected file"):
                    regenerate.apply_patch(inputs, patch)
                self.assertEqual(target.read_bytes(), original)
                self.assertFalse((inputs / "extra.csv").exists())
            patch.write_bytes(patch_bytes(old="明治"))
            with mock.patch.object(regenerate.subprocess, "run", wraps=subprocess.run) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    regenerate.apply_patch(inputs, patch)
                self.assertEqual(len(run.call_args_list), 2)
            self.assertEqual(target.read_bytes(), original)

    def test_invalid_patch_download_or_cache_is_never_published_or_replaced(self):
        data = patch_bytes()
        manifest = {"dictionary_patch": {"path": "Noun.proper.csv.patch", "target": "Noun.proper.csv",
                    "url": "https://example.invalid/patch", "bytes": len(data), "sha256": digest(data)}}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "Noun.proper.csv.patch"
            with mock.patch.object(regenerate, "urlopen", return_value=io.BytesIO(data.decode("euc-jp").encode("utf-8"))):
                with self.assertRaisesRegex(RuntimeError, "Downloaded.*checksum"):
                    regenerate.prepare_patch(manifest, root, False)
            self.assertEqual(list(root.iterdir()), [])
            target.write_bytes(b"changed")
            with mock.patch.object(regenerate, "urlopen") as download:
                with self.assertRaisesRegex(RuntimeError, "Cached.*checksum"):
                    regenerate.prepare_patch(manifest, root, False)
                download.assert_not_called()
            self.assertEqual(target.read_bytes(), b"changed")

    def test_auxiliary_resources_are_checked_alongside_dictionaries(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            resource = "org/apache/lucene/analysis/ja/stopwords.txt"
            manifest = {"lucene_version": "test", "dictionary_resources": [], "analysis_resources": [
                {"path": resource, "bytes": 3, "sha256": digest(b"abc")}]}
            jar = root / "lucene-analysis-kuromoji-test.jar"
            with ZipFile(jar, "w") as archive:
                archive.writestr(resource, b"abd")
            with self.assertRaisesRegex(RuntimeError, "resource checksum"):
                runtime.verify_dictionary_resources(manifest, root)
            with ZipFile(jar, "a") as archive:
                archive.writestr("org/apache/lucene/analysis/ja/extra.txt", b"abc")
            with self.assertRaisesRegex(RuntimeError, "resource inventory"):
                runtime.verify_dictionary_resources(manifest, root)

    def test_failed_builder_cannot_publish_output_or_replace_reviewed_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            provenance = root / "csv_manifest.json"
            provenance.write_text("retain")
            manifest = json.loads((ROOT / "manifest.json").read_text())
            with mock.patch.object(sys, "argv", ["regenerate_dictionary.py", "--output", str(output), "--write-manifest"]), \
                    mock.patch.object(regenerate, "MANIFEST_PATH", provenance), \
                    mock.patch.object(regenerate, "prepare_jars", return_value=manifest), \
                    mock.patch.object(regenerate, "verify_dictionary_resources"), \
                    mock.patch.object(regenerate, "prepare_source", return_value=provenance), \
                    mock.patch.object(regenerate, "prepare_patch", return_value=provenance), \
                    mock.patch.object(regenerate, "extract_inputs", return_value=[]), \
                    mock.patch.object(regenerate, "apply_patch"), \
                    mock.patch.object(regenerate.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "docker")):
                with self.assertRaises(subprocess.CalledProcessError):
                    regenerate.main()
            self.assertFalse(output.exists())
            self.assertEqual(provenance.read_text(), "retain")
            self.assertFalse(list(root.glob("uqa-kuromoji-csv-*")))

    def test_model_verification_mount_is_read_only_and_has_no_network(self):
        manifest = json.loads((ROOT / "manifest.json").read_text())
        args = mock.Mock(platform="linux/amd64", offline=True)
        result = subprocess.CompletedProcess([], 0, json.dumps({"runtime": manifest["runtime"]}))
        with mock.patch.object(sys.modules["lucene_model"].subprocess, "run", return_value=result) as run:
            export.EXPORTER.run_model(manifest, Path("/cache"), args, Path("/model"), True)
        command = run.call_args.args[0]
        self.assertIn("type=bind,source=/model,target=/output,readonly", command)
        self.assertEqual(command[command.index("--network") + 1], "none")
        self.assertEqual(command[command.index("--pull") + 1], "never")
        self.assertEqual(command[-3:], ["/src/KuromojiModel.java", "verify", "/output"])


if __name__ == "__main__":
    unittest.main()
