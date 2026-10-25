#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture named EXPLAIN inputs and execution effects from PostgreSQL 18."""

import json
import os
import subprocess


BASE = "CREATE TEMP VIEW named_input AS SELECT n FROM (VALUES (1),(2),(3)) input(n);"
EFFECT = "CREATE TEMP SEQUENCE named_effect; CREATE TEMP VIEW named_input AS SELECT n, nextval('named_effect') AS effect FROM (VALUES (1),(2),(3)) input(n);"
CASES = [
    ("view", BASE, "EXPLAIN (FORMAT JSON) SELECT n FROM named_input", None),
    ("nested_view", BASE + "CREATE TEMP VIEW named_outer AS SELECT n FROM named_input LIMIT 2;", "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) SELECT n FROM named_outer", None),
    ("folded_cte", "", "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) WITH input AS NOT MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM input", None),
    ("lexical_view_cte", "CREATE TEMP VIEW named_input AS WITH input AS MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM input;", "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) WITH input AS MATERIALIZED (VALUES (9)) SELECT * FROM named_input", None),
    ("unexecuted_effect", EFFECT, "EXPLAIN (FORMAT JSON) SELECT effect FROM named_input", "SELECT nextval('named_effect')"),
    ("executed_effect", EFFECT, "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) SELECT effect FROM named_input", "SELECT nextval('named_effect')"),
    ("unused_volatile_output", EFFECT, "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) SELECT n FROM named_input", "SELECT nextval('named_effect')"),
    ("prepared_view", BASE + "PREPARE named_statement(integer) AS SELECT n FROM named_input LIMIT $1;", "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) EXECUTE named_statement(2)", None),
]


def capture():
    command = [*json.loads(os.environ["PG_EXPLAIN_COMMAND"]), "-XqAt", "-v", "ON_ERROR_STOP=1"]

    def query(sql):
        return subprocess.check_output(command, input=sql, text=True).strip()

    version = query("SELECT version();")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    for name, setup, sql, after in CASES:
        text = query("BEGIN;\n" + setup + "\n" + sql + ";\n" + (after + ";\n" if after else "") + "ROLLBACK;")
        document, end = json.JSONDecoder().raw_decode(text)
        plan = document[0]["Plan"]
        remainder = text[end:].strip()
        if not after and remainder:
            raise RuntimeError((name, remainder))
        cases.append({
            "name": name, "setup": setup, "sql": sql, "after": after,
            "rows": plan["Plan Rows"], "width": plan["Plan Width"],
            "actual_rows": plan.get("Actual Rows"),
            "next_value": int(remainder) if after else None,
        })
    return {"postgresql_version": version, "cases": cases}


if __name__ == "__main__":
    print(json.dumps(capture(), indent=2))
