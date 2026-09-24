#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Refresh compact numeric comparison expectations from a PostgreSQL 18 container."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-sql/src/expr/binary/comparison/pg18.json"
OPERATORS = ["=", "<>", "<", "<=", ">", ">="]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=pathlib.Path, default=FIXTURE)
    args = parser.parse_args()
    command = [
        "docker", "exec", args.container, "psql", "-X", "-q", "-U", "postgres",
        "-A", "-t", "-F", "\t", "-P", "null=\\N", "-v", "ON_ERROR_STOP=1",
        "-v", "VERBOSITY=verbose",
    ]

    def query(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command + ["-c", sql], capture_output=True, text=True, timeout=30)

    version = query("SELECT version();")
    version.check_returncode()
    if not version.stdout.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    image = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True
    ).strip()
    source_fixture = json.loads(FIXTURE.read_text())
    cases = []
    for source in source_fixture["cases"]:
        left, right = source["left"], source["right"]
        expressions = [f"({left}) {operator} ({right})" for operator in OPERATORS]
        sql = "SELECT " + ", ".join(expressions + [f"pg_typeof({expressions[0]})::text"]) + ";"
        result = query(sql)
        case = {"left": left, "right": right}
        if result.returncode:
            error = re.search(r"ERROR:\s+([0-9A-Z]{5}):", result.stderr)
            if not error:
                raise RuntimeError(result.stderr or result.stdout)
            case["sqlstate"] = error.group(1)
        else:
            cells = result.stdout.rstrip("\n").split("\t")
            if len(cells) != 7 or cells[-1] != "boolean":
                raise RuntimeError(f"unexpected oracle row: {result.stdout!r}")
            case["type"] = cells[-1]
            case["values"] = [{"t": True, "f": False, "\\N": None}[cell] for cell in cells[:-1]]
        cases.append(case)
    relations = source_fixture["relations"]
    setup = ";".join(relations["setup"]) + ";"
    relation_queries = []
    for source in relations["queries"]:
        sql = source["sql"]
        result = query(f"BEGIN; {setup} SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM ({sql}) t; ROLLBACK;")
        result.check_returncode()
        rows = [list(row.values()) for row in json.loads(result.stdout)]
        relation_queries.append({"sql": sql, "rows": rows})
    successful = []
    unique = []
    for source in relations["unique"]:
        sql = source["sql"]
        result = query("BEGIN; " + ";".join(successful + [sql, "ROLLBACK"]) + ";")
        case = {"sql": sql}
        if result.returncode:
            error = re.search(r"ERROR:\s+([0-9A-Z]{5}):", result.stderr)
            if not error:
                raise RuntimeError(result.stderr or result.stdout)
            case["sqlstate"] = error.group(1)
        else:
            successful.append(sql)
        unique.append(case)
    # Each expected pair occupies one line; machine diagnostics stay out of the fixture.
    metadata = {"postgresql": version.stdout.strip(), "image": image, "operators": OPERATORS}
    lines = ["{"]
    lines.extend(f"  {json.dumps(key)}: {json.dumps(value)}," for key, value in metadata.items())
    lines.append('  "cases": [')
    lines.extend("    " + json.dumps(case) + ("," if index + 1 < len(cases) else "") for index, case in enumerate(cases))
    lines.append("  ],")
    lines.append('  "relations": {')
    for index, (key, rows) in enumerate([
        ("setup", relations["setup"]), ("queries", relation_queries), ("unique", unique)
    ]):
        lines.append(f'    "{key}": [')
        lines.extend("      " + json.dumps(row) + ("," if number + 1 < len(rows) else "") for number, row in enumerate(rows))
        lines.append("    ]" + ("," if index < 2 else ""))
    lines.extend(["  }", "}"])
    args.output.write_text("\n".join(lines) + "\n")
    print(f"Captured {len(cases)} pairs / {len(cases) * len(OPERATORS)} operator expectations from {version.stdout.split(' (')[0]}.")


if __name__ == "__main__":
    main()
