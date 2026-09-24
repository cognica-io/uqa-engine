#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture PostgreSQL assignment targets, row images and diagnostics in Docker."""

from __future__ import annotations

import argparse
import csv
import io
import json
from pathlib import Path
import re
import subprocess


HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "assignment_subscripts.expected.json"
STORED = "SELECT id, value::text AS value, array_dims(value) AS dimensions FROM assignment_target ORDER BY id"
NULL = "__UQA_ASSIGNMENT_NULL__"


def projected_rows(output: str) -> list[dict]:
    reader = csv.DictReader(io.StringIO(output))
    if not reader.fieldnames or set(reader.fieldnames) - {"id", "value", "dimensions", "total"}:
        raise RuntimeError(f"unexpected oracle projection: {reader.fieldnames}")
    return [{name: None if value == NULL else int(value) if name in {"id", "total"} else value
             for name, value in row.items()} for row in reader]


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
    for name, target, value in [
        ("multidimensional_flat_slice_source", "value[1:2][2:2]", "ARRAY[8,9,10]"),
        ("multidimensional_omitted_dimension", "value[2:2]", "ARRAY[8,9,10]"),
        ("multidimensional_extension", "value[3][1]", "9"),
        ("wrong_subscript_count", "value[1]", "9"),
    ]:
        add(name, f"UPDATE assignment_target SET {target}={value}", [
            setup[0], "INSERT INTO assignment_target VALUES (1,ARRAY[[1,2],[3,4]])",
        ])
    add("shifted_multidimensional_element", "UPDATE assignment_target SET value[1][0]=9", [
        setup[0], "INSERT INTO assignment_target VALUES (1,'[0:1][-1:0]={{1,2},{3,4}}')",
    ])
    add("slice_extends_with_gap", "UPDATE assignment_target SET value[5:6]=ARRAY[8,9]")
    for name, target, value in [
        ("empty_slice_missing_bound", "value[:2]", "ARRAY[8,9]"),
        ("empty_zero_width_slice", "value[3:2]", "ARRAY[1]"),
        ("empty_negative_width_slice", "value[3:1]", "ARRAY[1]"),
        ("empty_lower_bound_limit", "value[2147483647]", "9"),
        ("empty_source_error_precedence", "value[2147483647:2147483647]", "'{}'::integer[]"),
    ]:
        add(name, f"UPDATE assignment_target SET {target}={value}", [
            setup[0], "INSERT INTO assignment_target VALUES (1,'{}'::integer[])",
        ])
    add("nonempty_bounds_error_precedence", "UPDATE assignment_target SET value[2147483647:2147483647]='{}'::integer[]", [
        setup[0], "INSERT INTO assignment_target VALUES (1,'[2147483646:2147483646]={1}')",
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
    add("bound_reads_original_row", "UPDATE assignment_target SET value[1]=3,value[value[1]]=9")
    add("repeated_same_element", "UPDATE assignment_target SET value[1]=7,value[1]=9")
    add("decimal_index_rounding", "UPDATE assignment_target SET value[1.6]=9")
    add("bound_scalar_subquery", "UPDATE assignment_target SET value[(SELECT 2)]=9")
    add("update_from_bounds", "UPDATE assignment_target AS t SET value[s.i]=s.n FROM (VALUES (2,9)) AS s(i,n)")
    add("mixed_slice_and_index", "UPDATE assignment_target SET value[1:2][2]=ARRAY[8,9,10,11]", [
        setup[0], "INSERT INTO assignment_target VALUES (1,ARRAY[[1,2],[3,4]])",
    ])
    add("insert_ignores_whole_column_default", "INSERT INTO assignment_target(id,value[3]) VALUES(2,9)", [
        "CREATE TABLE assignment_target (id integer PRIMARY KEY, value integer[] DEFAULT ARRAY[1,2,3])",
    ])
    add("partial_default_rejected", "UPDATE assignment_target SET value[1]=DEFAULT WHERE false")
    add("null_slice_null_bound", "UPDATE assignment_target SET value[NULL:2]=NULL")
    add("domain_repeated_elements", "UPDATE assignment_target SET value[1]=3,value[2]=4", [
        "CREATE DOMAIN assignment_items AS integer[] CHECK(value[1]<value[2])",
        "CREATE TABLE assignment_target(id integer PRIMARY KEY,value assignment_items)",
        "INSERT INTO assignment_target VALUES(1,ARRAY[1,2])",
    ])
    add("domain_null_slice_insert", "INSERT INTO assignment_target(id,value[1:2]) VALUES(1,NULL)", [
        "CREATE DOMAIN assignment_items AS integer[] NOT NULL",
        "CREATE TABLE assignment_target(id integer PRIMARY KEY,value assignment_items)",
    ])
    add("automatic_view_repeated_elements", "UPDATE assignment_view SET value[1]=value[2],value[2]=value[1]", setup + [
        "CREATE VIEW assignment_view AS SELECT id,value FROM assignment_target",
    ])
    add("automatic_view_insert", "INSERT INTO assignment_view(id,value[1],value[3]) VALUES(2,7,9)", setup + [
        "CREATE VIEW assignment_view AS SELECT id,value FROM assignment_target",
    ])

    def stored(name: str, sql: str, before: list[str], projection: str = STORED) -> None:
        output.append({"name": name, "setup": before, "sql": sql,
                       "stored_sql": projection, "reopen_before": True})

    function = "CREATE FUNCTION assignment_patch(target_slot integer, replacement integer) RETURNS TABLE(id integer,value text,dimensions text) LANGUAGE SQL BEGIN ATOMIC UPDATE assignment_target SET value[target_slot]=replacement RETURNING id,value::text,array_dims(value); END"
    stored("stored_function_element", "SELECT * FROM assignment_patch(2,9)", setup + [function])
    renamed = STORED.replace("value::text", "payload::text").replace("array_dims(value)", "array_dims(payload)")
    stored("stored_function_target_rename", "SELECT * FROM assignment_patch(2,9)", setup + [
        function,
        "ALTER TABLE assignment_target RENAME COLUMN value TO payload",
        "ALTER TABLE assignment_target ADD COLUMN value integer[] DEFAULT ARRAY[90,91,92]",
    ], renamed)
    stored("stored_function_bound_rename", "SELECT * FROM assignment_patch(9)", setup + [
        "ALTER TABLE assignment_target ADD COLUMN slot integer DEFAULT 2",
        "CREATE FUNCTION assignment_patch(replacement integer) RETURNS TABLE(id integer,value text,dimensions text) LANGUAGE SQL BEGIN ATOMIC UPDATE assignment_target SET value[slot]=replacement RETURNING id,value::text,array_dims(value); END",
        "ALTER TABLE assignment_target RENAME COLUMN slot TO retained_slot",
        "ALTER TABLE assignment_target ADD COLUMN slot integer DEFAULT 3",
    ])
    stored("stored_function_bound_routine_rename", "SELECT * FROM assignment_patch(9)", setup + [
        "CREATE FUNCTION assignment_position() RETURNS integer LANGUAGE SQL IMMUTABLE RETURN 2",
        "CREATE FUNCTION assignment_patch(replacement integer) RETURNS TABLE(id integer,value text,dimensions text) LANGUAGE SQL BEGIN ATOMIC UPDATE assignment_target SET value[assignment_position()]=replacement RETURNING id,value::text,array_dims(value); END",
        "ALTER FUNCTION assignment_position() RENAME TO retained_position",
        "CREATE FUNCTION assignment_position() RETURNS integer LANGUAGE SQL IMMUTABLE RETURN 3",
    ])
    rule_setup = setup + [
        "CREATE TABLE assignment_command(id integer,slot integer,replacement integer)",
        "CREATE RULE assignment_rule AS ON INSERT TO assignment_command DO ALSO UPDATE assignment_target SET value[new.slot]=new.replacement WHERE assignment_target.id=new.id",
    ]
    command = "INSERT INTO assignment_command VALUES(1,2,9) RETURNING id,replacement::text AS value,NULL::text AS dimensions"
    stored("stored_rule_element", command, rule_setup)
    stored("stored_rule_target_rename", command, rule_setup + [
        "ALTER TABLE assignment_target RENAME COLUMN value TO payload",
        "ALTER TABLE assignment_target ADD COLUMN value integer[] DEFAULT ARRAY[90,91,92]",
    ], renamed)
    add("generated_value_after_partial_assignment", "UPDATE assignment_target SET value[1]=7,value[2]=9", setup + [
        "ALTER TABLE assignment_target ADD COLUMN total integer GENERATED ALWAYS AS (value[1]+value[2]) STORED",
    ])
    output[-1]["stored_sql"] = STORED.replace(" AS dimensions", " AS dimensions,total")
    add("domain_multiple_rows_atomic", "UPDATE assignment_target SET value[2]=value[2]+7", [
        "CREATE DOMAIN assignment_items AS integer[] CHECK(value[2]<10)",
        "CREATE TABLE assignment_target(id integer PRIMARY KEY,value assignment_items)",
        "INSERT INTO assignment_target VALUES(1,ARRAY[1,2,3]),(2,ARRAY[4,5,6])",
    ])
    add("unique_multiple_rows_atomic", "UPDATE assignment_target SET value[2]=9", setup + [
        "INSERT INTO assignment_target VALUES(2,ARRAY[4,5,6])",
        "CREATE UNIQUE INDEX assignment_element_key ON assignment_target((value[2]))",
    ])
    add("trigger_assignment_failure", "UPDATE assignment_target SET value[2]=9", setup + [
        "CREATE FUNCTION assignment_reject() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'assignment rejected'; END $$",
        "CREATE TRIGGER assignment_reject AFTER UPDATE ON assignment_target FOR EACH ROW EXECUTE FUNCTION assignment_reject()",
    ])
    for case in output:
        if case["name"] in {"domain_multiple_rows_atomic", "unique_multiple_rows_atomic", "trigger_assignment_failure"}:
            case["savepoint"] = True
    unique_setup = setup + [
        "INSERT INTO assignment_target VALUES(2,ARRAY[4,5,6])",
        "CREATE UNIQUE INDEX assignment_element_key ON assignment_target((value[2]))",
        "CREATE ROLE assignment_writer",
        "GRANT UPDATE(value),SELECT(id,value) ON assignment_target TO assignment_writer",
    ]
    for name, grants in [("unique_expression_column_privileges", []),
                         ("unique_expression_table_privilege", ["GRANT SELECT ON assignment_target TO assignment_writer"])]:
        add(name, "UPDATE assignment_target SET value[2]=9", unique_setup + grants)
        output[-1]["role"] = "assignment_writer"
    add("unique_column_privileges", "UPDATE assignment_target SET value[2]=9", setup + [
        "INSERT INTO assignment_target VALUES(2,ARRAY[1,5,3])",
        "CREATE UNIQUE INDEX assignment_array_key ON assignment_target(value)",
        "CREATE ROLE assignment_writer",
        "GRANT UPDATE(value),SELECT(id,value) ON assignment_target TO assignment_writer",
    ])
    output[-1]["role"] = "assignment_writer"
    add("unique_real_output", "UPDATE assignment_target SET value[1]=1.1", [
        "CREATE TABLE assignment_target(id integer PRIMARY KEY,value real[])",
        "INSERT INTO assignment_target VALUES(1,ARRAY[1.2,2.3]),(2,ARRAY[3.4,4.5])",
        "CREATE UNIQUE INDEX assignment_real_key ON assignment_target((value[1]))",
    ])
    for name, ty, initial in [("unique_fixed_char_output", "character(3)", "'xy'"),
                              ("unique_regclass_output", "regclass", "42::oid::regclass"),
                              ("unique_int2vector_output", "int2vector", "'1 2'"),
                              ("unique_oidvector_output", "oidvector", "'1 2'")]:
        add(name, "UPDATE assignment_target SET value[2]=9", setup + [
            "INSERT INTO assignment_target VALUES(2,ARRAY[4,5,6])",
            f"ALTER TABLE assignment_target ADD COLUMN label {ty} DEFAULT {initial}",
            "CREATE UNIQUE INDEX assignment_composite_key ON assignment_target((value[2]),label) INCLUDE(id)",
        ])
    add("unique_boolean_output", "UPDATE assignment_target SET value[1]=true", [
        "CREATE TABLE assignment_target(id integer PRIMARY KEY,value boolean[])",
        "INSERT INTO assignment_target VALUES(1,ARRAY[true,false]),(2,ARRAY[false,true])",
        "CREATE UNIQUE INDEX assignment_bool_key ON assignment_target((value[1]))",
    ])
    add("unique_null_output", "UPDATE assignment_target SET value[2]=NULL", setup + [
        "INSERT INTO assignment_target VALUES(2,ARRAY[4,5,6])",
        "CREATE UNIQUE INDEX assignment_null_key ON assignment_target((value[2])) NULLS NOT DISTINCT",
    ])
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=Path, default=FIXTURE)
    arguments = parser.parse_args()
    command = ["docker", "exec", "-i", arguments.container, "psql", "-X", "-q", "-U", "postgres",
               "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose"]

    def query(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command + ["-A", "-t", "-c", sql], capture_output=True, text=True, timeout=30)

    version = query("SELECT version()")
    version.check_returncode()
    if not version.stdout.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    captured = cases()
    for case in captured:
        # Execute the original statement directly: wrapping rule-backed DML in a CTE changes PostgreSQL's accepted syntax.
        sql = "BEGIN; " + "; ".join(case["setup"]) + ";\n"
        if case.get("role"):
            sql += "SET LOCAL ROLE " + case["role"] + ";\n"
        if case.get("savepoint"):
            sql += "SAVEPOINT assignment_undo;\n"
        sql += "\\echo UQA_ASSIGNMENT_OPERATION\n" + case["sql"] + ";\n"
        sql += "\\echo UQA_ASSIGNMENT_STORED\n" + case.get("stored_sql", STORED) + ";\n"
        sql += "\\echo UQA_ASSIGNMENT_END\nROLLBACK;\n"
        result = subprocess.run(command + ["--csv", "-P", f"null={NULL}"], input=sql,
                                capture_output=True, text=True, timeout=30)
        _, marker, output = result.stdout.partition("UQA_ASSIGNMENT_OPERATION\n")
        if not marker:
            raise RuntimeError(f"{case['name']}: setup did not complete: {result.stderr}")
        if result.returncode:
            if "UQA_ASSIGNMENT_STORED\n" in output:
                raise RuntimeError(f"{case['name']}: operation succeeded but stored-row capture failed: {result.stderr}")
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
            operation, marker, output = output.partition("UQA_ASSIGNMENT_STORED\n")
            stored, end, remainder = output.partition("UQA_ASSIGNMENT_END\n")
            if not marker or not end or remainder.strip():
                raise RuntimeError(f"{case['name']}: expected operation and stored rows: {result.stdout}")
            case["rows"] = projected_rows(operation)
            case["stored"] = projected_rows(stored)
    fixture = {
        "postgresql": version.stdout.strip(),
        "image": subprocess.check_output(["docker", "inspect", "--format", "{{.Image}}", arguments.container], text=True).strip(),
        "cases": captured,
    }
    arguments.output.write_text(json.dumps(fixture, indent=2, ensure_ascii=False) + "\n")
    print(f"Captured {len(captured)} PostgreSQL assignment target cases")


if __name__ == "__main__":
    main()
