#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture ordered-set mode equality classes directly from PostgreSQL 18."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-execution/src/aggregation/tests/pg18_mode.json"

# Winners are captured from PostgreSQL, never calculated from the inputs here.
CASES = [
    ("empty", "int", []),
    ("all_null", "float8", [None, None]),
    ("singleton", "int", ["2"]),
    ("integer_tie", "int", ["2", "1"]),
    ("repeated_tie", "int", ["3", "1", "2", "3", "2", "1"]),
    ("middle_winner", "int", ["3", "2", "1", "2", "2"]),
    ("last_winner", "int", ["3", "2", "3", "1", "3"]),
    ("merge_runs", "int", ["3", "2", "1"] * 8),
    ("signed_zero", "float8", ["-0", "0", "-0", "0", "1", "1"]),
    ("signed_zero_tie", "float8", ["1", "-0", "1", "0"]),
    ("nan_winner", "float8", ["NaN", "1", "NaN", "1", "NaN"]),
    ("nonfinite_tie", "float8", ["NaN", "Infinity", "-Infinity", "0"]),
    ("numeric_scale", "numeric", ["1.0", "2", "1.00", "2", "1", "3"]),
    ("numeric_precision", "numeric", ["0.1", "0.1000000000000000055511151231257827021181583404541015625"]),
    ("numeric_nan", "numeric", ["NaN", "1", "NaN", "1", "NaN"]),
    ("interval_month_days", "interval", ["1 month", "30 days", "1 month", "30 days", "2 months", "2 months"]),
    ("interval_day_hours", "interval", ["1 day", "24 hours", "1 day", "24 hours", "2 days", "2 days"]),
    ("interval_negative", "interval", ["-1 month", "-30 days", "-1 month", "-30 days", "-2 months", "-2 months"]),
    ("time_endpoints", "time", ["24:00", "00:00"]),
    ("timetz_offset_tie", "timetz", ["12:00+00", "13:00+01"]),
    ("timetz_no_wrap", "timetz", ["22:30+00", "00:30+02"]),
    ("jsonb_zero", "jsonb", ["0.1", "0", "-0.0", "0.1", "0e2"]),
    ("jsonb_numeric_scale", "jsonb", ["1.0", "2", "1.00", "2", "1e0"]),
    ("jsonb_objects", "jsonb", ['{"a":1,"b":2}', '{"b":2.0,"a":1e0}', '{"a":2}', '{"a":1.00,"b":2}', '{"a":2}']),
    ("jsonb_arrays", "jsonb", ["[0]", "[-0.0]", "[1]", "[0e2]", "[1]"]),
    ("bpchar_padding", "bpchar", ["a", "a ", "b", "a  ", "b"]),
    ("text_tie", "text", ["beta", "alpha", "gamma"]),
    ("nulls_ignored", "int", [None, "2", None, "1", None]),
    ("array_signed_zero", "float8[]", ["{-0,NULL}", "{0,NULL}", "{1,NULL}", "{-0,NULL}", "{1,NULL}"]),
]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=pathlib.Path, default=FIXTURE)
    args = parser.parse_args()
    command = ["docker", "exec", args.container, "psql", "-X", "-q", "-U", "postgres",
               "-A", "-t", "-v", "ON_ERROR_STOP=1"]

    def query(sql: str) -> str:
        result = subprocess.run(command + ["-c", sql], capture_output=True, text=True,
                                timeout=30, check=True)
        return result.stdout.strip()

    version = query("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    image = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True
    ).strip()
    cases = []
    for name, kind, values in CASES:
        literals = ["NULL" if value is None else "'" + value.replace("'", "''") + "'"
                    for value in values]
        inputs = "VALUES " + ",".join(f"({i},{v}::{kind})" for i, v in enumerate(literals))
        if not values:
            inputs = f"SELECT 0,NULL::{kind} WHERE false"
        for direction in ("ASC", "DESC"):
            sql = (f"WITH inputs(i,v) AS ({inputs}) SELECT min(i) AS winner FROM inputs "
                   f"WHERE v = (SELECT mode() WITHIN GROUP (ORDER BY v {direction}) FROM inputs)")
            rows = json.loads(query("SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (" + sql + ") t"))
            cases.append({"name": name, "type": kind, "values": values, "descending": direction == "DESC",
                          "sql": sql, "winner": rows[0]["winner"]})
    lines = ["{", f'  "postgresql": {json.dumps(version)},', f'  "image": {json.dumps(image)},',
             '  "source": "https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/adt/orderedsetaggs.c",',
             '  "cases": [']
    lines.extend("    " + json.dumps(case) + ("," if i + 1 < len(cases) else "")
                 for i, case in enumerate(cases))
    lines.extend(["  ]", "}"])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines) + "\n")
    print(f"Captured {len(cases)} mode outcomes from {version.split(' (')[0]}.")


if __name__ == "__main__":
    main()
