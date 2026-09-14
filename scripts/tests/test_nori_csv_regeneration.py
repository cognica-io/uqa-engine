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
import tarfile
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2] / "tests/parity/nori"
sys.path.insert(0, str(ROOT))
try:
    spec = importlib.util.spec_from_file_location("nori_csv_regeneration", ROOT / "regenerate_dictionary.py")
    regenerate = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(regenerate)
finally:
    sys.path.pop(0)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_archive(path, entries):
    with tarfile.open(path, "w:gz") as archive:
        for name, kind in entries:
            entry = tarfile.TarInfo(name)
            entry.type = kind
            entry.size = 3 if kind == tarfile.REGTYPE else 0
            entry.linkname = "elsewhere" if kind == tarfile.SYMTYPE else ""
            archive.addfile(entry, io.BytesIO(b"abc") if entry.size else None)


class NoriCsvRegenerationTest(unittest.TestCase):
    def test_reviewed_inputs_and_generated_resources_are_complete(self):
        actual = json.loads((ROOT / "csv_manifest.json").read_text())
        reference = json.loads((ROOT / "manifest.json").read_text())
        self.assertEqual(actual["reference_manifest_sha256"], regenerate.sha256(ROOT / "manifest.json"))
        self.assertEqual(actual["builder_source_sha256"], regenerate.sha256(ROOT / regenerate.ENTRYPOINT))
        self.assertEqual(actual["resources"], sorted(reference["dictionary_resources"], key=lambda row: row["path"]))
        self.assertEqual(actual["source_archive"]["sha256"], reference["dictionary_source"]["sha256"])
        self.assertEqual(actual["runtime"], reference["runtime"])
        self.assertFalse(actual["builder"]["normalize_entries"])
        self.assertEqual(len(actual["inputs"]), 43)
        self.assertEqual(len({row["path"] for row in actual["inputs"]}), 43)

    def test_extracts_only_direct_builder_inputs_in_stable_order(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "inputs"
            output.mkdir()
            archive = root / "source.tgz"
            names = ["z.csv", "a.csv", "matrix.def", "char.def", "unk.def", "configure", "user/dictionary.csv"]
            write_archive(archive, [("source/" + name, tarfile.REGTYPE) for name in names])
            rows = regenerate.extract_inputs(archive, "source", output)
            self.assertEqual([row["path"] for row in rows], ["a.csv", "char.def", "matrix.def", "unk.def", "z.csv"])
            self.assertTrue(all(row["bytes"] == 3 and row["sha256"] == digest(b"abc") for row in rows))

    def test_rejects_path_traversal_links_duplicates_and_missing_inputs(self):
        cases = [
            [("../escape.csv", tarfile.REGTYPE)],
            [("/escape.csv", tarfile.REGTYPE)],
            [("source/a.csv", tarfile.SYMTYPE)],
            [("source/a.csv", tarfile.REGTYPE)] * 2,
            [("source/a.csv", tarfile.REGTYPE)],
        ]
        for entries in cases:
            with self.subTest(entries=entries), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                archive = root / "source.tgz"
                output = root / "input"
                output.mkdir()
                write_archive(archive, entries)
                with self.assertRaises(RuntimeError):
                    regenerate.extract_inputs(archive, "source", output)

    def test_corrupted_cached_or_downloaded_source_is_not_published_or_replaced(self):
        manifest = {"dictionary_source": {"name": "source", "url": "https://example.invalid/source", "sha256": digest(b"abc")}}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "source.tar.gz"
            with mock.patch.object(regenerate, "urlopen", return_value=io.BytesIO(b"abd")):
                with self.assertRaisesRegex(RuntimeError, "Downloaded dictionary source checksum"):
                    regenerate.prepare_source(manifest, root, False)
            self.assertEqual(list(root.iterdir()), [])
            target.write_bytes(b"abd")
            with mock.patch.object(regenerate, "urlopen") as download:
                with self.assertRaisesRegex(RuntimeError, "Cached dictionary source checksum"):
                    regenerate.prepare_source(manifest, root, False)
                download.assert_not_called()
            self.assertEqual(target.read_bytes(), b"abd")

    def test_same_size_resource_change_and_extra_resource_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "dictionary.dat").write_bytes(b"abc")
            expected = regenerate.inventory(root)
            (root / "dictionary.dat").write_bytes(b"abd")
            with self.assertRaisesRegex(RuntimeError, "dictionary.dat"):
                regenerate.check_resources(root, expected)
            (root / "dictionary.dat").write_bytes(b"abc")
            (root / "extra.dat").write_bytes(b"")
            with self.assertRaisesRegex(RuntimeError, "extra.dat"):
                regenerate.check_resources(root, expected)

    def test_existing_output_is_preserved_before_preparation(self):
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.object(sys, "argv", ["regenerate_dictionary.py", "--output", temporary]):
                with mock.patch.object(regenerate, "prepare_jars") as prepare:
                    with self.assertRaisesRegex(RuntimeError, "Output already exists"):
                        regenerate.main()
                    prepare.assert_not_called()

    def test_failed_build_or_resource_check_cannot_publish_output_or_rewrite_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "source.tgz"
            write_archive(archive, [("source/" + name, tarfile.REGTYPE) for name in ["a.csv", *regenerate.DEFINITIONS]])
            expected = root / "csv_manifest.json"
            expected.write_text("retain")
            manifest = json.loads((ROOT / "manifest.json").read_text())
            manifest["dictionary_source"]["name"] = "source"
            output = root / "output"
            runtime = "\n".join(manifest["runtime"][name] for name in regenerate.RUNTIME_FIELDS) + "\n"
            failures = [
                (subprocess.CalledProcessError(1, "docker"), subprocess.CalledProcessError),
                (subprocess.CompletedProcess([], 0, "wrong\nruntime\nidentity\n"), RuntimeError),
                (subprocess.CompletedProcess([], 0, "missing fields\n"), RuntimeError),
                (subprocess.CompletedProcess([], 0, runtime + "extra field\n"), RuntimeError),
                (subprocess.CompletedProcess([], 0, runtime), RuntimeError),
            ]
            for result, error in failures:
                with self.subTest(result=result), \
                        mock.patch.object(sys, "argv", ["regenerate_dictionary.py", "--output", str(output), "--write-manifest"]), \
                        mock.patch.object(regenerate, "MANIFEST_PATH", expected), \
                        mock.patch.object(regenerate, "prepare_jars", return_value=manifest), \
                        mock.patch.object(regenerate, "verify_dictionary_resources"), \
                        mock.patch.object(regenerate, "prepare_source", return_value=archive), \
                        mock.patch.object(regenerate.subprocess, "run", side_effect=[result]):
                    with self.assertRaises(error):
                        regenerate.main()
                self.assertFalse(output.exists())
                self.assertEqual(expected.read_text(), "retain")
                self.assertFalse(list(root.glob("uqa-nori-csv-*")))

    def test_docker_uses_the_pinned_jvm_and_read_only_input_mount(self):
        manifest = json.loads((ROOT / "manifest.json").read_text())
        command = regenerate.docker_command(manifest, Path("/cache"), "linux/amd64", True,
                                           regenerate.ENTRYPOINT, ("/input", "/output"),
                                           output=Path("/result"), input_directory=Path("/source"))
        self.assertEqual(command[:3], ["docker", "run", "--rm"])
        self.assertIn(manifest["docker_image"], command)
        self.assertIn("type=bind,source=/source,target=/input,readonly", command)
        self.assertEqual(command[command.index("--network") + 1], "none")
        self.assertEqual(command[command.index("--pull") + 1], "never")
        self.assertIn("-Xmx1g", command)


if __name__ == "__main__":
    unittest.main()
