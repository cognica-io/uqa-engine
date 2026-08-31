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
    path = ROOT / "scripts" / "check-rust-file-lines.py"
    spec = importlib.util.spec_from_file_location("uqa_check_rust_file_lines", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


CHECKER = load_checker()


def entry(path: str, lines: int) -> dict[str, object]:
    return {
        "path": path,
        "baseline_lines": lines,
        "owner": "test owner",
        "responsibility_groups": ["test responsibility"],
        "target_modules": ["crates/example/src/target.rs"],
        "migration_state": "planned",
    }


def write_lines(path: pathlib.Path, count: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("fn line() {}\n" * count, encoding="utf-8")


def write_policy(root: pathlib.Path, entries: list[dict[str, object]]) -> pathlib.Path:
    path = root / "scripts" / "rust-file-line-policy.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "line_limit": 1000,
                "excluded_roots": ["crates/imported"],
                "oversized_files": entries,
            }
        ),
        encoding="utf-8",
    )
    return path


class RustFileLinePolicyTest(unittest.TestCase):
    def test_accepts_exact_inventory_and_reports_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = "crates/example/src/lib.rs"
            write_lines(root / source, 1001)
            policy = write_policy(root, [entry(source, 1001)])

            result = CHECKER.verify(root, policy)

            self.assertEqual(result["inventoried_files"], 1)
            self.assertEqual(result["files_at_or_above_limit"], 1)
            self.assertEqual(result["threshold_counts"]["1000"], 1)

    def test_rejects_inventory_growth(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = "crates/example/src/lib.rs"
            write_lines(root / source, 1002)
            policy = write_policy(root, [entry(source, 1001)])

            with self.assertRaisesRegex(CHECKER.PolicyError, "grew beyond its ratchet"):
                CHECKER.verify(root, policy)

    def test_requires_lower_ratchet_after_shrink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = "crates/example/src/lib.rs"
            write_lines(root / source, 1000)
            policy = write_policy(root, [entry(source, 1001)])

            with self.assertRaisesRegex(CHECKER.PolicyError, "lower the ratchet"):
                CHECKER.verify(root, policy)

    def test_requires_resolved_entry_removal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = "crates/example/src/lib.rs"
            write_lines(root / source, 999)
            policy = write_policy(root, [entry(source, 1001)])

            with self.assertRaisesRegex(CHECKER.PolicyError, "must be removed"):
                CHECKER.verify(root, policy)

    def test_rejects_new_file_at_inventory_threshold(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = "crates/example/src/new.rs"
            write_lines(root / source, 1000)
            policy = write_policy(root, [])

            with self.assertRaisesRegex(CHECKER.PolicyError, "is not inventoried"):
                CHECKER.verify(root, policy)

    def test_ignores_imported_and_target_sources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            write_lines(root / "crates/imported/src/generated.rs", 2000)
            write_lines(root / "fuzz/target/build/generated.rs", 2000)
            write_lines(root / "crates/example/src/lib.rs", 10)
            policy = write_policy(root, [])

            result = CHECKER.verify(root, policy)

            self.assertEqual(result["rust_files"], 1)


if __name__ == "__main__":
    unittest.main()
