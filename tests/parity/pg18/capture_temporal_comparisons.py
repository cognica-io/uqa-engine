#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture TIME/TIMETZ comparisons and relational results from PostgreSQL 18."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-core/src/types/tests/pg18_temporal.json"


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
        kind = group["kind"]
        if kind not in ("time", "timetz"):
            raise ValueError(kind)
        values = ",".join(f"({i},{kind} '{value.replace(chr(39), chr(39)*2)}')" for i, value in enumerate(group["values"]))
        group["comparisons"] = rows(
            f"WITH vals(i,v) AS (VALUES {values}) SELECT a.i AS left_id,b.i AS right_id,a.v=b.v AS eq,a.v<b.v AS lt,a.v>b.v AS gt FROM vals a CROSS JOIN vals b ORDER BY a.i,b.i", []
        )
    for case in source["queries"]:
        case["rows"] = rows(case["sql"], source["setup"])
    for group in source["unique"]:
        accepted = list(group["setup"])
        for case in group["commands"]:
            result = query("BEGIN; " + ";".join(accepted + [case["sql"]]) + "; ROLLBACK;")
            if result.returncode:
                error = re.search(r"ERROR:\s+([0-9A-Z]{5}):", result.stderr)
                if not error:
                    raise RuntimeError(result.stderr)
                case["sqlstate"] = error.group(1)
            else:
                case.pop("sqlstate", None)
                accepted.append(case["sql"])
    lines = ["{"]
    for key in ("postgresql", "image"):
        lines.append(f"  {json.dumps(key)}: {json.dumps(source[key])},")
    for index, key in enumerate(("types", "setup", "queries", "unique")):
        lines.append(f"  {json.dumps(key)}: [")
        values = source[key]
        lines.extend("    " + json.dumps(value) + ("," if i+1 < len(values) else "") for i, value in enumerate(values))
        lines.append("  ]" + ("," if index < 3 else ""))
    lines.append("}")
    args.output.write_text("\n".join(lines) + "\n")
    count = sum(len(group["comparisons"]) * 3 for group in source["types"])
    print(f"Captured {count} scalar outcomes and {len(source['queries'])} relational queries from {version.stdout.split(' (')[0]}.")


if __name__ == "__main__":
    main()
