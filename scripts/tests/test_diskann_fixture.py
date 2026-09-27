#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check immutable vector-source selection with independently specified IEEE-754 bytes."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "freeze-diskann-fixture.py"
SPEC = importlib.util.spec_from_file_location("freeze_diskann_fixture", SCRIPT)
FREEZER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FREEZER)


class DiskANNFixtureTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)
        self.expected = {
            "corpus.f32": bytes.fromhex("0000803f000080bf0000000000000040"),
            "queries.f32": bytes.fromhex("0000803f00000000"),
        }
        sources = {
            "corpus": [
                {"source_id": "a", "embedding": [1, -1]},
                {"source_id": "b", "embedding": [0, 2]},
                {"source_id": "c", "embedding": [3, 4]},
            ],
            "queries": [{"id": "q", "embedding": [1, 0]}],
        }
        self.prepared = {"dataset": {"name": "literal"}, "embedding": {"dimensions": 2}, "artifacts": {}}
        for kind, rows in sources.items():
            path = self.root / f"{kind}.jsonl"
            path.write_text("".join(json.dumps(row) + "\n" for row in rows))
            self.prepared["artifacts"][kind] = {
                "path": path.name, "rows": len(rows),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        self.specification = {
            "schema_version": 1,
            "encoding": "row-major little-endian IEEE-754 float32",
            "dataset": self.prepared["dataset"],
            "embedding": self.prepared["embedding"],
            "prepared_sources": copy.deepcopy(self.prepared["artifacts"]),
            "dimensions": 2,
            "artifacts": {},
        }
        for kind, identities in [("corpus", ["a", "b"]), ("queries", ["q"])]:
            payload = self.expected[f"{kind}.f32"]
            self.specification["artifacts"][kind] = {
                "path": f"{kind}.f32", "rows": len(identities), "source_ids": identities,
                "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
            }
        self.write_prepared()

    def write_prepared(self):
        (self.root / "prepared-manifest.json").write_text(json.dumps(self.prepared))

    def test_prefix_selection_and_independent_binary_values(self):
        self.assertEqual(FREEZER.fixture_bytes(self.specification, self.root), self.expected)

    def test_source_identity_and_output_drift_are_rejected(self):
        for category, changed in [
            ("dataset", {"name": "different"}),
            ("encoding", "native-endian float32"),
            ("prepared_sources", {}),
            ("artifacts.corpus", {"source_ids": ["b", "a"]}),
            ("artifacts.corpus", {"path": "../escape"}),
            ("artifacts.corpus", {"path": ".."}),
            ("artifacts.corpus", {"sha256": "0" * 64}),
            ("artifacts.corpus", {"bytes": 8}),
            ("artifacts.queries", {"path": "corpus.f32"}),
        ]:
            with self.subTest(category=category, changed=changed), self.assertRaises(ValueError):
                specification = copy.deepcopy(self.specification)
                if category.startswith("artifacts."):
                    specification["artifacts"][category.split(".")[1]].update(changed)
                else:
                    specification[category] = changed
                FREEZER.fixture_bytes(specification, self.root)
        (self.root / "corpus.jsonl").write_text("{}\n")
        with self.assertRaisesRegex(ValueError, "content hash"):
            FREEZER.fixture_bytes(self.specification, self.root)

    def test_invalid_coordinates_cannot_be_frozen(self):
        for values in ([1], [1, True], [1, float("nan")], [1, float("inf")], [1, 1e100]):
            with self.subTest(values=values), self.assertRaises(ValueError):
                path = self.root / "queries.jsonl"
                path.write_text(json.dumps({"id": "q", "embedding": values}) + "\n")
                self.prepared["artifacts"]["queries"]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                self.write_prepared()
                specification = copy.deepcopy(self.specification)
                specification["prepared_sources"] = copy.deepcopy(self.prepared["artifacts"])
                FREEZER.fixture_bytes(specification, self.root)


if __name__ == "__main__":
    unittest.main()
