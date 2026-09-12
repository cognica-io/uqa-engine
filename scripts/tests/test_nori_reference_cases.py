#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2] / "tests/parity/nori"
sys.path.insert(0, str(ROOT))
try:
    spec = importlib.util.spec_from_file_location("nori_reference_cases", ROOT / "reference_cases.py")
    reference = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reference)
finally:
    sys.path.pop(0)


class NoriReferenceCasesTest(unittest.TestCase):
    def test_checked_in_sources_and_complete_case_inventory_match_reviewed_hashes(self):
        for stem, entrypoint in [("user", "NoriUserReference.java"), ("tokenizer", "NoriTokenizerReference.java"), ("analysis", "NoriAnalysisReference.java"), ("number", "NoriNumberReference.java"), ("generic", "NoriNumberReference.java")]:
            cases = json.loads((ROOT / f"{stem}_cases.json").read_text(encoding="utf-8"))
            output = (ROOT / f"{stem}_expected.jsonl").read_text(encoding="utf-8")
            expected = json.loads((ROOT / f"{stem}_manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(reference.provenance(stem, entrypoint, cases, output), expected)
            self.assertEqual(reference.inventory(cases), [json.loads(row)["id"] for row in output.split("\n") if row])

    def test_unicode_line_terminators_and_unpaired_surrogates_survive_json_transport(self):
        value = {"id": "raw", "text": "a\u0085b\u2028c\u2029d\ud800"}
        stdout = '{"runtime":{}}\n' + json.dumps(value, ensure_ascii=False) + "\n"
        output = reference.canonical_output(stdout, {}, ["raw"])
        self.assertTrue(output.isascii())
        self.assertEqual(json.loads(output), value)

    def test_mismatched_runtime_and_missing_or_reordered_cases_are_rejected(self):
        for stdout in [
            '{"runtime":{"changed":true}}\n{"id":"a"}\n{"id":"b"}\n',
            '{"runtime":{}}\n{"id":"a"}\n',
            '{"runtime":{}}\n{"id":"b"}\n{"id":"a"}\n',
        ]:
            with self.assertRaisesRegex(RuntimeError, "runtime or case inventory"):
                reference.canonical_output(stdout, {}, ["a", "b"])
        for cases in [[], [{"id": "a"}, {"id": "a"}], [{"id": "a\tb"}]]:
            with self.assertRaises(RuntimeError):
                reference.inventory(cases)

    def test_changed_fixture_fails_before_any_download_or_jvm_and_is_not_rewritten(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ["user_cases.json", "user_expected.jsonl", "user_manifest.json", "manifest.json", "NoriUserReference.java"]:
                (root / name).write_bytes((ROOT / name).read_bytes())
            expected = root / "user_expected.jsonl"
            expected.write_bytes(expected.read_bytes() + b"\n")
            changed = expected.read_bytes()
            with mock.patch.object(reference, "ROOT", root), mock.patch.object(sys, "argv", ["run_user_reference.py"]):
                with mock.patch.object(reference, "prepare_jars") as prepare, mock.patch.object(reference.subprocess, "run") as run:
                    with self.assertRaisesRegex(RuntimeError, "provenance changed"):
                        reference.main("user", "NoriUserReference.java", lambda case: [], "test")
                    prepare.assert_not_called()
                    run.assert_not_called()
            self.assertEqual(expected.read_bytes(), changed)


if __name__ == "__main__":
    unittest.main()
