#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture legacy vector ordering, dimensions and domain behavior from PostgreSQL 18."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-core/src/types/tests/pg18_legacy_vectors.json"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=pathlib.Path, default=FIXTURE)
    args = parser.parse_args()
    command = ["docker", "exec", args.container, "psql", "-X", "-q", "-U", "postgres",
               "-A", "-t", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose"]

    def query(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command + ["-c", sql], capture_output=True, text=True, timeout=30)

    def rows(sql: str, setup: list[str]) -> list:
        result = query("BEGIN; " + ";".join(setup) + "; SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (" + sql + ") t; ROLLBACK;")
        result.check_returncode()
        return [list(row.values()) for row in json.loads(result.stdout)]

    def rejection(case: dict, setup: list[str]) -> None:
        result = query("BEGIN; " + ";".join(setup + [case["sql"]]) + "; ROLLBACK;")
        error = re.search(r"ERROR:\s+([0-9A-Z]{5}):\s*([^\n]*)", result.stderr)
        if not result.returncode or not error:
            raise RuntimeError(f"expected PostgreSQL rejection: {case['sql']}\n{result.stderr}")
        case["sqlstate"] = error.group(1)
        if "message" in case:
            case["message"] = error.group(2)

    source = json.loads(FIXTURE.read_text())
    version = query("SELECT version();")
    version.check_returncode()
    if not version.stdout.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    source["postgresql"] = version.stdout.strip()
    source["image"] = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True
    ).strip()
    for group in source["types"]:
        ty = group["type"]
        values = ",".join(f"({i},'{value.replace(chr(39), chr(39)*2)}'::{ty})" for i, value in enumerate(group["values"]))
        group["comparisons"] = rows(f"WITH vals(i,v) AS (VALUES {values}) SELECT a.i AS left_id,b.i AS right_id,a.v=b.v AS eq,a.v<b.v AS lt,a.v>b.v AS gt FROM vals a CROSS JOIN vals b ORDER BY a.i,b.i", [])
        for case in group["queries"]:
            case["rows"] = rows(case["sql"], group["setup"])
        for case in group["rejected"]:
            rejection(case, [])
        for case in group["unique"]:
            rejection(case, case["setup"])
        setup = list(group["setup"])
        for update in group["updates"]:
            setup.append(update["sql"])
            for case in update["queries"]:
                case["rows"] = rows(case["sql"], setup)
    lines = ["{", f"  \"postgresql\": {json.dumps(source['postgresql'])},",
             f"  \"image\": {json.dumps(source['image'])},", "  \"types\": ["]
    for index, group in enumerate(source["types"]):
        lines.append("    {")
        for key in ("type", "array_type", "values"):
            lines.append(f"      {json.dumps(key)}: {json.dumps(group[key])},")
        fields = ("comparisons", "setup", "queries", "rejected", "unique", "updates")
        for field_index, key in enumerate(fields):
            values = group[key]
            lines.append(f"      {json.dumps(key)}: [")
            lines.extend("        " + json.dumps(value) + ("," if i+1 < len(values) else "") for i, value in enumerate(values))
            lines.append("      ]" + ("," if field_index + 1 < len(fields) else ""))
        lines.append("    }" + ("," if index+1 < len(source["types"]) else ""))
    lines.extend(["  ]", "}"])
    args.output.write_text("\n".join(lines) + "\n")
    outcomes = sum(len(group["comparisons"]) * 3 for group in source["types"])
    print(f"Captured {outcomes} comparison outcomes and legacy-vector SQL acceptance cases.")


if __name__ == "__main__":
    main()
