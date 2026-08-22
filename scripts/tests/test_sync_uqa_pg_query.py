#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import argparse
import importlib.util
import pathlib
import sys
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "sync-uqa-pg-query.py"
SPEC = importlib.util.spec_from_file_location("sync_uqa_pg_query", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
SYNC = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SYNC
SPEC.loader.exec_module(SYNC)


class SyncUQAPGQueryTest(unittest.TestCase):
    def test_format_imported_rust_uses_the_cargo_format_graph(self) -> None:
        with mock.patch.object(SYNC, "run") as run:
            SYNC.format_imported_rust()

        run.assert_called_once_with(
            ["cargo", "fmt", "--package", "uqa-pg-query"],
            cwd=SYNC.ROOT,
        )

    def test_import_is_formatted_before_checksums_are_written(self) -> None:
        imported = [("src/lib.rs", pathlib.Path("src/lib.rs"))]
        events: list[str] = []
        with (
            mock.patch.object(SYNC, "format_imported_rust") as format_imported,
            mock.patch.object(SYNC, "write_checksums") as write_checksums,
        ):
            format_imported.side_effect = lambda: events.append("format")
            write_checksums.side_effect = lambda _imported: events.append("checksums")
            SYNC.finalize_import(imported)

        self.assertEqual(events, ["format", "checksums"])
        format_imported.assert_called_once_with()
        write_checksums.assert_called_once_with(imported)

    def test_check_mode_does_not_reformat_the_snapshot(self) -> None:
        arguments = argparse.Namespace(check=True, source=None)
        with (
            mock.patch.object(SYNC, "parse_args", return_value=arguments),
            mock.patch.object(SYNC, "check_tree") as check_tree,
            mock.patch.object(SYNC, "parse_checksums", return_value=[]),
            mock.patch.object(SYNC, "format_imported_rust") as format_imported,
            mock.patch("builtins.print"),
        ):
            self.assertEqual(SYNC.main(), 0)

        check_tree.assert_called_once_with()
        format_imported.assert_not_called()


if __name__ == "__main__":
    unittest.main()
