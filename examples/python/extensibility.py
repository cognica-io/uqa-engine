#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run the extensibility scenario through the Python binding."""

from __future__ import annotations

import json
from typing import Dict, List, Optional

import uqa


def normalize_label(value: str) -> str:
    return value.strip().lower().replace(" ", "-")


def repeat_rows(label: str, times: int) -> dict[str, object]:
    return {
        "columns": ["label", "idx"],
        "rows": [[label, index] for index in range(times)],
    }


class SumSquares:
    def __init__(self) -> None:
        self.total = 0

    def observe(self, value: Optional[int]) -> None:
        if value is not None:
            self.total += value * value

    def finish(self) -> int:
        return self.total


def main() -> None:
    engine = uqa.Engine()
    try:
        engine.sql("CREATE TABLE samples (grp TEXT, label TEXT, value INTEGER)")
        engine.sql(
            "INSERT INTO samples (grp, label, value) VALUES "
            "('a', ' SQL Manual ', 1), ('a', 'Node JS', 2), ('b', 'Browser WASM', 3)"
        )
        options = {"volatility": "immutable", "may_mutate_engine": False}
        engine.register_scalar_function("normalize_label", normalize_label, **options)
        engine.register_table_function("repeat_rows", repeat_rows, **options)
        engine.register_aggregate_function("sum_squares", SumSquares, **options)

        results = {
            "scalar": engine.sql(
                "SELECT normalize_label(label) AS label FROM samples ORDER BY value"
            ).rows,
            "table": engine.sql(
                "SELECT label, idx FROM repeat_rows('row', 3) AS r(label, idx) ORDER BY idx"
            ).rows,
            "aggregate": engine.sql(
                "SELECT grp, sum_squares(value) AS total FROM samples GROUP BY grp ORDER BY grp"
            ).rows,
        }
        assert results == expected_results()
        print(json.dumps(results, sort_keys=True))
    finally:
        engine.close()


def expected_results() -> Dict[str, List[Dict[str, object]]]:
    return {
        "scalar": [
            {"label": "sql-manual"},
            {"label": "node-js"},
            {"label": "browser-wasm"},
        ],
        "table": [
            {"label": "row", "idx": 0},
            {"label": "row", "idx": 1},
            {"label": "row", "idx": 2},
        ],
        "aggregate": [{"grp": "a", "total": 5}, {"grp": "b", "total": 9}],
    }


if __name__ == "__main__":
    main()
