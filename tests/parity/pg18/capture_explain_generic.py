#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture unbound EXPLAIN parameters and independent prepared parameter hooks."""

import json
import os

from capture_explain_options import capture


def probes(record, cases):
    record("sql", sql="CREATE TEMP TABLE generic_items (n integer)")
    options = [
        "GENERIC_PLAN", "GENERIC_PLAN FALSE",
        "GENERIC_PLAN FALSE, GENERIC_PLAN TRUE",
        "GENERIC_PLAN TRUE, GENERIC_PLAN FALSE",
        "GENERIC_PLAN bogus", "GENERIC_PLAN 2",
        "GENERIC_PLAN, ANALYZE", "GENERIC_PLAN, unknown_option",
    ]
    bodies = [
        "SELECT $1::integer + 1", "SELECT $1", "SELECT $2::integer",
        "SELECT $1 + $2", "SELECT $1 IS NULL", "SELECT missing_column",
        "SELECT $1 IS NULL, $1::integer", "SELECT $1::integer, $1 IS NULL",
        "SELECT n FROM generic_items WHERE n = $1",
        "INSERT INTO generic_items VALUES ($2) RETURNING n",
        "UPDATE generic_items SET n = $2 WHERE n = $1 RETURNING n",
        "WITH source AS (SELECT $2::integer AS n) SELECT n FROM source",
    ]
    for option in options:
        for body in bodies:
            record("sql", sql=f"EXPLAIN ({option}, FORMAT JSON) {body}")
    record("sql", sql="SELECT count(*) FROM generic_items")
    for index, (types, body) in enumerate(
        (types, body) for types in ([], ["integer"], ["unknown"])
        for body in ("SELECT $1", "SELECT $1 IS NULL", "SELECT $2::integer", "SELECT 1", "")
    ):
        prepared_name = f"generic_{index}"
        sql = f"EXPLAIN (GENERIC_PLAN, FORMAT JSON) {body}" if body else ""
        record("prepare", name=prepared_name, sql=sql, types=types)
        if cases[-1]["result"]["status"] != 1:
            continue
        record("describe", name=prepared_name)
        count = len(cases[-1]["result"]["parameters"])
        record("execute", name=prepared_name, parameters=["7"] * count)
    for sql in [
        "BEGIN", "SAVEPOINT generic_retry",
        "EXPLAIN (GENERIC_PLAN) SELECT $2::integer",
        "EXPLAIN (GENERIC_PLAN) SELECT $1 + $2",
        "SELECT 1", "ROLLBACK TO generic_retry",
        "EXPLAIN (GENERIC_PLAN) SELECT $1 IS NULL", "COMMIT",
    ]:
        record("sql", sql=sql)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_EXPLAIN_CONNECTION"], probes), indent=2))
