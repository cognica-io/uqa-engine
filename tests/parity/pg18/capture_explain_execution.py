#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture root execution counts and timing field presence from PostgreSQL 18."""

import hashlib
import json
import os
from pathlib import Path
import subprocess


TABLE = "CREATE TEMP TABLE measured_input(n integer); INSERT INTO measured_input VALUES (1),(2),(3);"
CASES = [
    ("constant", "", "SELECT 1"),
    ("empty", "", "SELECT 1 WHERE false"),
    ("values", "", "SELECT * FROM (VALUES (1),(2),(3)) input(n)"),
    ("empty_aggregate", "", "SELECT count(*) FROM (SELECT 1 WHERE false) input"),
    ("limit_zero", "", "SELECT * FROM generate_series(1,3) LIMIT 0"),
    ("union_all", "", "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3"),
    ("table", TABLE, "SELECT n FROM measured_input"),
    ("ctas", TABLE, "CREATE TEMP TABLE measured_output AS SELECT n FROM measured_input"),
    ("ctas_empty", TABLE, "CREATE TEMP TABLE measured_output AS SELECT n FROM measured_input WHERE false"),
    ("ctas_no_data", TABLE, "CREATE TEMP TABLE measured_output AS SELECT n FROM measured_input WITH NO DATA"),
    ("ctas_locked", TABLE, "CREATE TEMP TABLE measured_output AS SELECT n FROM measured_input FOR UPDATE"),
    ("materialized_view", "", "CREATE MATERIALIZED VIEW measured_output AS VALUES (1),(2),(3)"),
    ("materialized_no_data", "", "CREATE MATERIALIZED VIEW measured_output AS VALUES (1),(2),(3) WITH NO DATA"),
    ("insert", TABLE, "INSERT INTO measured_input VALUES (4),(5)"),
    ("insert_returning", TABLE, "INSERT INTO measured_input VALUES (4),(5) RETURNING n"),
    ("update", TABLE, "UPDATE measured_input SET n=n+1"),
    ("update_returning", TABLE, "UPDATE measured_input SET n=n+1 RETURNING n"),
    ("delete", TABLE, "DELETE FROM measured_input"),
    ("delete_returning", TABLE, "DELETE FROM measured_input RETURNING n"),
    ("prepared", "PREPARE measured_statement(integer) AS SELECT n FROM generate_series(1,$1) input(n);", "EXECUTE measured_statement(3)"),
]


def capture():
    command = [*json.loads(os.environ["PG_EXPLAIN_COMMAND"]), "-XqAt", "-v", "ON_ERROR_STOP=1"]

    def query(sql):
        return subprocess.check_output(command, input=sql, text=True).strip()

    version = query("SELECT version();")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    for name, setup, body in CASES:
        for timing in (False, True):
            sql = f"EXPLAIN (ANALYZE, TIMING {str(timing).upper()}, SUMMARY FALSE, BUFFERS FALSE, FORMAT JSON) {body}"
            document = json.loads(query(f"BEGIN; {setup}\n{sql};\nROLLBACK;"))[0]
            plan = document["Plan"]
            cases.append({
                "name": f"{name}_{'timed' if timing else 'untimed'}", "setup": setup, "sql": sql,
                "rows": plan["Actual Rows"], "loops": plan["Actual Loops"],
                "startup": "Actual Startup Time" in plan, "total": "Actual Total Time" in plan,
                "summary": "Execution Time" in document,
            })
    return {
        "postgresql_version": version,
        "capture_command": command,
        "collector_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "cases": cases,
    }


if __name__ == "__main__":
    print(json.dumps(capture(), indent=2))
