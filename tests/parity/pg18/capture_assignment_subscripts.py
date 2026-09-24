#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture PostgreSQL assignment targets, row images and diagnostics in Docker."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess


HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "assignment_subscripts.expected.json"


def cases() -> list[dict]:
    setup = [
        "CREATE TABLE assignment_target (id integer PRIMARY KEY, value integer[])",
        "INSERT INTO assignment_target VALUES (1, ARRAY[1,2,3])",
    ]
    returning = " RETURNING id, value::text AS value, array_dims(value) AS dimensions"
    output = []

    def add(name: str, sql: str, before: list[str] | None = None) -> None:
        projection = returning.replace("RETURNING id", "RETURNING t.id") if sql.startswith("MERGE ") else returning
        output.append({"name": name, "setup": setup if before is None else before,
                       "sql": sql + projection})

    for name, target, value in [
        ("element", "value[2]", "9"),
        ("element_null", "value[2]", "NULL"),
        ("extend_before", "value[-1]", "9"),
        ("extend_after", "value[5]", "9"),
        ("slice", "value[2:3]", "ARRAY[8,9]"),
        ("slice_lower_omitted", "value[:2]", "ARRAY[8,9]"),
        ("slice_upper_omitted", "value[2:]", "ARRAY[8,9]"),
        ("slice_null", "value[2:3]", "NULL"),
        ("element_array_rhs", "value[2]", "ARRAY[9]"),
        ("slice_scalar_rhs", "value[2:3]", "9"),
        ("null_index", "value[NULL]", "9"),
        ("text_index", "value['bad']", "9"),
        ("reversed_slice", "value[3:2]", "ARRAY[9]"),
    ]:
        add(name, f"UPDATE assignment_target SET {target}={value} WHERE id=1")
    for name, initial in [("null_array", "NULL"), ("empty_array", "'{}'::integer[]")]:
        add(name, "UPDATE assignment_target SET value[2]=9", [
            setup[0], f"INSERT INTO assignment_target VALUES (1,{initial})",
        ])
    add("unvisited_null_index", "UPDATE assignment_target SET value[NULL]=9 WHERE false")
    add("repeated_element_targets", "UPDATE assignment_target SET value[1]=value[2],value[2]=value[1]")
    add("whole_and_element_targets", "UPDATE assignment_target SET value=ARRAY[7],value[2]=9")
    add("multidimensional_element", "UPDATE assignment_target SET value[2][1]=9", [
        setup[0], "INSERT INTO assignment_target VALUES (1,ARRAY[[1,2],[3,4]])",
    ])
    add("multidimensional_slice", "UPDATE assignment_target SET value[1:2][2:2]=ARRAY[[8],[9]]", [
        setup[0], "INSERT INTO assignment_target VALUES (1,ARRAY[[1,2],[3,4]])",
    ])
    add("insert_element", "INSERT INTO assignment_target(id,value[3]) VALUES(2,9)")
    add("insert_repeated_elements", "INSERT INTO assignment_target(id,value[1],value[3]) VALUES(2,7,9)")
    add("insert_whole_and_element", "INSERT INTO assignment_target(id,value,value[2]) VALUES(2,ARRAY[7],9)")
    add("insert_select", "INSERT INTO assignment_target(id,value[3]) SELECT 2,9")
    add("conflict_element", "INSERT INTO assignment_target VALUES(1,ARRAY[7,8]) ON CONFLICT(id) DO UPDATE SET value[2]=excluded.value[1]")
    add("merge_update", "MERGE INTO assignment_target AS t USING (VALUES(1,9)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET value[2]=s.n")
    add("merge_insert", "MERGE INTO assignment_target AS t USING (VALUES(2,9)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT(id,value[2]) VALUES(s.id,s.n)")
    for ty in ["int2vector", "oidvector"]:
        add(f"{ty}_element", "UPDATE assignment_target SET value[0]=9", [
            f"CREATE TABLE assignment_target(id integer PRIMARY KEY,value {ty})",
            "INSERT INTO assignment_target VALUES(1,'1 2')",
        ])
    for name, replacement in [("domain_accepts", "9"), ("domain_rejects", "99")]:
        add(name, f"UPDATE assignment_target SET value[2]={replacement}", [
            "CREATE DOMAIN assignment_items AS integer[] CHECK(value[2]<10)",
            "CREATE TABLE assignment_target(id integer PRIMARY KEY,value assignment_items)",
            "INSERT INTO assignment_target VALUES(1,ARRAY[1,2,3])",
        ])
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=Path, default=FIXTURE)
    arguments = parser.parse_args()
    command = ["docker", "exec", arguments.container, "psql", "-X", "-q", "-U", "postgres",
               "-A", "-t", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose"]

    def query(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command + ["-c", sql], capture_output=True, text=True, timeout=30)

    version = query("SELECT version()")
    version.check_returncode()
    if not version.stdout.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    captured = cases()
    for case in captured:
        sql = "BEGIN; " + "; ".join(case["setup"]) + "; "
        sql += "WITH changed AS (" + case["sql"] + ") SELECT coalesce(json_agg(row_to_json(changed)),'[]'::json) FROM changed; ROLLBACK;"
        result = query(sql)
        if result.returncode:
            error = re.search(r"ERROR:\s+([0-9A-Z]{5}):\s*([^\n]*)", result.stderr)
            if not error:
                raise RuntimeError(f"{case['name']}: {result.stderr}")
            diagnostic = {"sqlstate": error.group(1), "message": error.group(2)}
            for field in ["DETAIL", "HINT"]:
                match = re.search(rf"^{field}:\s*(.*)$", result.stderr, re.MULTILINE)
                if match:
                    diagnostic[field.lower()] = match.group(1)
            case["error"] = diagnostic
        else:
            case["rows"] = json.loads(result.stdout)
    fixture = {
        "postgresql": version.stdout.strip(),
        "image": subprocess.check_output(["docker", "inspect", "--format", "{{.Image}}", arguments.container], text=True).strip(),
        "cases": captured,
    }
    arguments.output.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n")
    print(f"Captured {len(captured)} PostgreSQL assignment target cases")


if __name__ == "__main__":
    main()
