#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_checker() -> object:
    path = ROOT / "scripts" / "check-engine-capabilities.py"
    spec = importlib.util.spec_from_file_location("uqa_check_engine_capabilities", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


CHECKER = load_checker()


def write_policy(
    root: pathlib.Path,
    leaf_source: str,
    adapter_source: str,
    allow_adapter: bool = True,
) -> pathlib.Path:
    policy = root / "scripts" / "engine-capability-policy.json"
    policy.parent.mkdir(parents=True, exist_ok=True)
    policy.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "capability_module": adapter_source,
                "scopes": [
                    {
                        "name": "migrated leaves",
                        "files": [leaf_source],
                        "engine_allowlist": [],
                    },
                    {
                        "name": "adapters",
                        "files": [adapter_source],
                        "engine_allowlist": [adapter_source] if allow_adapter else [],
                    },
                ],
            }
        ),
        encoding="utf-8",
    )
    return policy


class EngineCapabilityPolicyTest(unittest.TestCase):
    def test_accepts_engine_free_leaf_and_declared_adapter(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            leaf = "crates/example/src/leaf.rs"
            adapter = "crates/example/src/capabilities.rs"
            (root / leaf).parent.mkdir(parents=True)
            (root / leaf).write_text("fn read_catalog() {}\n", encoding="utf-8")
            (root / adapter).write_text("impl Engine {}\n", encoding="utf-8")
            policy = write_policy(root, leaf, adapter)

            result = CHECKER.verify(root, policy)

            self.assertEqual(result["checked_files"], 2)
            self.assertEqual(result["declared_engine_adapters"], 1)
            self.assertEqual(result["engine_free_leaf_files"], 1)

    def test_rejects_engine_reference_in_migrated_leaf(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            leaf = "crates/example/src/leaf.rs"
            adapter = "crates/example/src/capabilities.rs"
            (root / leaf).parent.mkdir(parents=True)
            (root / leaf).write_text("fn read(engine: &Engine) {}\n", encoding="utf-8")
            (root / adapter).write_text("impl Engine {}\n", encoding="utf-8")
            policy = write_policy(root, leaf, adapter)

            with self.assertRaisesRegex(CHECKER.PolicyError, "Engine reference is not allowed"):
                CHECKER.verify(root, policy)

    def test_rejects_stale_engine_allowlist_entry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            leaf = "crates/example/src/leaf.rs"
            adapter = "crates/example/src/capabilities.rs"
            (root / leaf).parent.mkdir(parents=True)
            (root / leaf).write_text("fn read_catalog() {}\n", encoding="utf-8")
            (root / adapter).write_text("fn build_capabilities() {}\n", encoding="utf-8")
            policy = write_policy(root, leaf, adapter)

            with self.assertRaisesRegex(CHECKER.PolicyError, "stale Engine allowlist"):
                CHECKER.verify(root, policy)

    def test_rejects_engine_reference_field_in_capability_module(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            leaf = "crates/example/src/leaf.rs"
            adapter = "crates/example/src/capabilities.rs"
            (root / leaf).parent.mkdir(parents=True)
            (root / leaf).write_text("fn read_catalog() {}\n", encoding="utf-8")
            (root / adapter).write_text(
                "struct CatalogView<'a> { engine: &'a Engine }\n", encoding="utf-8"
            )
            policy = write_policy(root, leaf, adapter)

            with self.assertRaisesRegex(CHECKER.PolicyError, "must not retain an engine"):
                CHECKER.verify(root, policy)

    def test_rejects_other_capability_escape_hatches(self) -> None:
        cases = (
            ("impl Deref for CatalogView {}\n", "must not implement or import Deref"),
            ("struct EngineServices;\n", "must not define a catch-all"),
            ("impl CatalogView { fn as_engine(&self) {} }\n", "must not expose an engine"),
        )
        for adapter_source, expected in cases:
            with self.subTest(adapter_source=adapter_source):
                with tempfile.TemporaryDirectory() as temporary:
                    root = pathlib.Path(temporary)
                    leaf = "crates/example/src/leaf.rs"
                    adapter = "crates/example/src/capabilities.rs"
                    (root / leaf).parent.mkdir(parents=True)
                    (root / leaf).write_text("fn read_catalog() {}\n", encoding="utf-8")
                    (root / adapter).write_text(adapter_source, encoding="utf-8")
                    policy = write_policy(root, leaf, adapter)

                    with self.assertRaisesRegex(CHECKER.PolicyError, expected):
                        CHECKER.verify(root, policy)

    def test_rejects_allowlist_entry_outside_scope(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            leaf = "crates/example/src/leaf.rs"
            adapter = "crates/example/src/capabilities.rs"
            policy = write_policy(root, leaf, adapter)
            document = json.loads(policy.read_text(encoding="utf-8"))
            document["scopes"][0]["engine_allowlist"] = [adapter]
            policy.write_text(json.dumps(document), encoding="utf-8")

            with self.assertRaisesRegex(CHECKER.PolicyError, "outside the scope"):
                CHECKER.load_policy(policy)


if __name__ == "__main__":
    unittest.main()
