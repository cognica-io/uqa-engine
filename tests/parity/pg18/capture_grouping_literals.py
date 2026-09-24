#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture grouping literal identity and diagnostics from disposable PostgreSQL 18."""

from __future__ import annotations

import argparse
import csv
import io
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-sql/src/semantics/aggregates/pg18_literals.json"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=pathlib.Path, default=FIXTURE)
    args = parser.parse_args()
    command = [
        "docker", "exec", "-i", args.container, "psql", "-X", "-q", "-U", "postgres",
        "--csv", "-P", "null=\\N", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose",
    ]

    def execute(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command, input=sql, capture_output=True, text=True, timeout=30)

    version = execute("SELECT version();")
    version.check_returncode()
    postgresql = list(csv.reader(io.StringIO(version.stdout)))[1][0]
    if not postgresql.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    image = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True
    ).strip()
    source = json.loads(FIXTURE.read_text())
    setup = ";\n".join(source["setup"]) + ";"
    cases = []
    for original in source["cases"]:
        sql = original["sql"]
        result = execute(f"BEGIN;\n{setup}\n\\echo UQA_CASE_BEGIN\n{sql};\nROLLBACK;\n")
        marker = "UQA_CASE_BEGIN\n"
        if marker not in result.stdout:
            raise RuntimeError(f"{original['name']}: setup failed: {result.stderr}")
        case = {"name": original["name"], "sql": sql}
        if result.returncode:
            error = re.search(r"ERROR:\s+([0-9A-Z]{5}): ([^\r\n]*)", result.stderr)
            if error is None:
                raise RuntimeError(result.stderr or result.stdout)
            case["sqlstate"], case["message"] = error.groups()
        else:
            if "ERROR:" in result.stderr:
                raise RuntimeError(result.stderr)
            records = list(csv.reader(io.StringIO(result.stdout.split(marker, 1)[1])))
            case["columns"] = records[0]
            case["rows"] = [
                [None if item == r"\N" else item for item in row] for row in records[1:]
            ]
        cases.append(case)
    source = {"postgresql": postgresql, "image": image, "setup": source["setup"], "cases": cases}
    args.output.write_text(json.dumps(source, indent=2) + "\n")
    print(f"Captured {len(cases)} grouping literal cases from {postgresql.split(' (')[0]}.")


if __name__ == "__main__":
    main()
