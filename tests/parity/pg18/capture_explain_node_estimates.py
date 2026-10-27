#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture finite CTE cardinalities through nested scopes from PostgreSQL 18."""

import hashlib
import json
import os
from pathlib import Path
import subprocess


CASES = [
    ("folded_repeated", "WITH x AS NOT MATERIALIZED (VALUES (1),(2),(3)) SELECT a.column1 FROM x a CROSS JOIN x b"),
    ("folded_definition_scope", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM (WITH a AS NOT MATERIALIZED (SELECT * FROM x), x AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM x CROSS JOIN a) s"),
    ("quoted", 'WITH "Quoted Name" AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM "Quoted Name"'),
    ("escaped_quote", 'WITH "quoted""name" AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM "quoted""name"'),
    ("quoted_dot", 'WITH "a.b" AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM "a.b"'),
    ("recursive_forward", "WITH RECURSIVE a AS MATERIALIZED (SELECT * FROM b), b AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM a"),
    ("unused", "WITH unused AS MATERIALIZED (VALUES (1),(2)) SELECT 7"),
    ("unused_volatile", "WITH unused AS MATERIALIZED (SELECT random()) SELECT 7"),
    ("preceding_scope", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM (WITH a AS MATERIALIZED (SELECT * FROM x), x AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM x CROSS JOIN a) s"),
    ("shadow", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM (WITH x AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM x) local_x CROSS JOIN x"),
    ("dependency", "WITH a AS MATERIALIZED (VALUES (1),(2)), b AS MATERIALIZED (SELECT * FROM a) SELECT * FROM b"),
    ("repeated", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)) SELECT * FROM x a CROSS JOIN x b"),
    ("blocking_repeated", "WITH x AS MATERIALIZED (SELECT * FROM (VALUES (1),(2),(3)) AS v(n) ORDER BY n) SELECT a.n FROM x a CROSS JOIN x b"),
    ("scalar", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)) SELECT (SELECT count(*) FROM x)"),
    ("nested_dependency", "WITH x AS MATERIALIZED (VALUES (1),(2),(3)), a AS MATERIALIZED (WITH x AS MATERIALIZED (VALUES (1),(2)) SELECT * FROM x) SELECT * FROM a CROSS JOIN x"),
]


def capture():
    command = [*json.loads(os.environ["PG_EXPLAIN_COMMAND"]), "-XqAt", "-v", "ON_ERROR_STOP=1"]

    def query(sql):
        return subprocess.check_output(command, input=sql, text=True).strip()

    version = query("SELECT version();")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    for name, body in CASES:
        sql = f"EXPLAIN (FORMAT JSON, SUMMARY FALSE, COSTS TRUE) {body}"
        plan = json.loads(query(sql))[0]["Plan"]
        scans = []
        pending = [plan]
        while pending:
            node = pending.pop()
            if node.get("Node Type") == "CTE Scan":
                scans.append({"relation": node["CTE Name"], "rows": node["Plan Rows"]})
            pending.extend(node.get("Plans", []))
        cases.append({
            "name": name, "sql": sql, "rows": plan["Plan Rows"],
            "cte_rows": sorted(scans, key=lambda node: (node["relation"], node["rows"])),
        })
    return {
        "postgresql_version": version,
        "capture_command": command,
        "collector_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "cases": cases,
    }


if __name__ == "__main__":
    print(json.dumps(capture(), indent=2))
