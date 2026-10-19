#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_script(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def timing_output(executions: dict[int, list[tuple[list[str], str]]]) -> str:
    lines = ["Timing is on."]
    for number in range(1, 23):
        lines.append(f"@@Q{number:02}")
        for printed, timing in executions.get(number, [([], "Time: 1.000 ms")] * 2):
            lines += printed
            lines.append(timing)
    lines.append("@@END")
    return "\n".join(lines) + "\n"


def explain_document(execution: float, planning: float) -> str:
    return json.dumps(
        [{"Plan": {"Node Type": "Result"}, "Planning Time": planning, "Execution Time": execution}],
        indent=2,
    )


class TPCHReleaseComparisonTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.script = load_script(
            "run_tpch_release_comparison", ROOT / "scripts" / "run-tpch-release-comparison.py"
        )

    def test_timings_follow_their_printed_rows(self) -> None:
        output = timing_output(
            {
                1: [(["A|F|1.50"], "Time: 2.500 ms"), (["A|F|1.50"], "Time: 2.250 ms")],
                2: [([], "Time: 1234.567 ms (00:01.235)"), ([], "Time: 3.000 ms")],
            }
        )
        samples, outputs = self.script.parse_timings(output, 2)
        self.assertEqual(samples[1], [2.5, 2.25])
        self.assertEqual(outputs[1], [("A|F|1.50",), ("A|F|1.50",)])
        self.assertEqual(samples[2], [1234.567, 3.0])
        self.assertEqual(samples[22], [1.0, 1.0])

    def test_lines_before_the_first_marker_are_ignored(self) -> None:
        output = "Time: 9.000 ms\n" + timing_output({})
        samples, _ = self.script.parse_timings(output, 2)
        self.assertEqual(samples[1], [1.0, 1.0])

    def test_missing_or_extra_timings_are_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "Q03 produced 1 timings; expected 2"):
            self.script.parse_timings(timing_output({3: [([], "Time: 1.000 ms")]}), 2)
        extra = [([], "Time: 1.000 ms")] * 3
        with self.assertRaisesRegex(RuntimeError, "Q04 produced 3 timings; expected 2"):
            self.script.parse_timings(timing_output({4: extra}), 2)

    def test_missing_query_and_duplicate_markers_are_rejected(self) -> None:
        output = timing_output({}).replace("@@Q22\n", "@@Q21\n", 1)
        with self.assertRaisesRegex(RuntimeError, "duplicate marker for Q21"):
            self.script.parse_timings(output, 2)
        truncated = timing_output({}).split("@@Q22\n", 1)[0]
        with self.assertRaisesRegex(RuntimeError, "Q22 produced 0 timings; expected 2"):
            self.script.parse_timings(truncated, 2)

    def test_explain_reads_execution_and_planning_times(self) -> None:
        lines = []
        for number in range(1, 23):
            lines.append(f"@@Q{number:02}")
            lines.append(explain_document(execution=number + 0.5, planning=0.125))
            lines.append(explain_document(execution=number + 0.25, planning=0.0625))
        lines.append("@@END")
        execution, planning = self.script.parse_explain("\n".join(lines) + "\n", 2)
        self.assertEqual(execution[1], [1.5, 1.25])
        self.assertEqual(execution[22], [22.5, 22.25])
        self.assertEqual(planning[7], [0.125, 0.0625])

    def test_explain_rejects_an_incomplete_query(self) -> None:
        lines = []
        for number in range(1, 23):
            lines.append(f"@@Q{number:02}")
            lines.append(explain_document(execution=1.0, planning=0.1))
        with self.assertRaisesRegex(RuntimeError, "Q01 produced 1 EXPLAIN documents; expected 2"):
            self.script.parse_explain("\n".join(lines), 2)

    def test_canonical_result_matches_the_postgresql_fixture_rules(self) -> None:
        result = {
            "columns": ["sum_qty", "flag", "label"],
            "rows": [["38.00", None, "0.50"], ["0.000", "A", "x  "]],
        }
        self.assertEqual(
            self.script.canonical_result(result, {0}),
            {"columns": ["sum_qty", "flag", "label"], "rows": [["38", "<NULL>", "0.50"], ["0", "A", "x  "]]},
        )

    def test_numeric_columns_follow_postgresql_type_names(self) -> None:
        description = [
            ("l_returnflag", "character(1)"),
            ("sum_qty", "numeric"),
            ("count_order", "bigint"),
            ("revenue", "numeric(15,2)"),
            ("o_orderdate", "date"),
            ("ratio", "double precision"),
        ]
        self.assertEqual(self.script.numeric_columns(description), {1, 2, 3, 5})

    def test_insert_statements_match_the_rust_loader_batches(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = pathlib.Path(directory)
            (fixture / "data").mkdir()
            for table, kinds in self.script.TABLES:
                count = 130 if table == "lineitem" else 1
                values = {"I": "7", "N": "1.50", "D": "1995-03-15", "T": "it's"}
                line = "|".join(values[kind] for kind in kinds) + "|\n"
                (fixture / "data" / f"{table}.tbl").write_text(line * count, encoding="utf-8")
            statements = self.script.insert_statements(fixture)
        self.assertEqual(len(statements), 9)
        self.assertEqual(statements[0], "INSERT INTO region VALUES\n(7, 'it''s', 'it''s');\n")
        self.assertTrue(statements[7].startswith("INSERT INTO lineitem VALUES\n(7, 7, 7, 7, 1.50, 1.50"))
        self.assertEqual(statements[7].count("\n("), 128)
        self.assertEqual(statements[8].count("\n("), 2)

    def test_rows_must_end_with_the_dbgen_separator(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = pathlib.Path(directory)
            (fixture / "data").mkdir()
            (fixture / "data" / "region.tbl").write_text("0|AFRICA|comment\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "does not end in '\\|'"):
                self.script.table_rows(fixture, "region")

    def test_checked_in_fixture_renders_every_row(self) -> None:
        statements = self.script.insert_statements(ROOT / "benchmarks" / "tpch")
        rendered_rows = sum(statement.count("\n(") for statement in statements)
        manifest = json.loads((ROOT / "benchmarks" / "tpch" / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(rendered_rows, sum(manifest["data"]["rows"].values()))

    def test_rounds_alternate_engine_order(self) -> None:
        self.assertEqual(self.script.round_order(0), ("uqa", "postgres_round_trip", "postgres_explain"))
        self.assertEqual(self.script.round_order(1), ("postgres_explain", "postgres_round_trip", "uqa"))
        self.assertEqual(self.script.round_order(2), self.script.round_order(0))

    def test_summary_table_reports_both_postgresql_boundaries(self) -> None:
        query = {
            "query": 6,
            "uqa": {"median_ms": 4.5},
            "postgres_round_trip": {"median_ms": 0.75},
            "postgres_execution": {"median_ms": 0.5},
        }
        table = self.script.summary_table([query]).splitlines()
        self.assertEqual(table[2], "| Q06 | 4.500 | 0.750 | 0.500 | 6.00x | 9.00x |")


if __name__ == "__main__":
    unittest.main()
