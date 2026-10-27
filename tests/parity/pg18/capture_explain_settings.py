#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture EXPLAIN's effective settings and summary visibility from PostgreSQL 18."""

import json
import os

from capture_command_completion_oracle import capture


QUERIES = [
    "SELECT version()",
    "EXPLAIN (SETTINGS, SUMMARY, FORMAT JSON) SELECT 1",
    "SET search_path TO pg_catalog",
    "SET work_mem TO '64kB'",
    "SET plan_cache_mode TO force_generic_plan",
    "SET enable_indexonlyscan TO off",
    "SET statement_timeout TO '20s'",
    "EXPLAIN (SETTINGS, SUMMARY, FORMAT JSON) SELECT 1",
    "EXPLAIN (SETTINGS FALSE, SUMMARY FALSE, FORMAT JSON) SELECT 1",
    "EXPLAIN (ANALYZE, SETTINGS, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) SELECT 1",
    "EXPLAIN (ANALYZE, SETTINGS, TIMING FALSE, FORMAT JSON) SELECT 1",
    "BEGIN",
    "SET LOCAL work_mem TO '128kB'",
    "EXPLAIN (SETTINGS, FORMAT JSON) SELECT 1",
    "ROLLBACK",
    "EXPLAIN (SETTINGS, FORMAT JSON) SELECT 1",
    "RESET ALL",
    "EXPLAIN (SETTINGS, FORMAT JSON) SELECT 1",
]


def observe(connection):
    # Match the engine's documented startup setting; RESET ALL restores this value while EXPLAIN still compares it with PostgreSQL's 4 MiB boot value.
    result = capture(QUERIES, connection + " options='-c work_mem=64MB'", include_rows=True)
    for case in result["cases"]:
        if case["error"]:
            raise RuntimeError(case)
        if not case["sql"].startswith("EXPLAIN"):
            continue
        output = case.pop("results")
        document = json.loads(output[0]["rows"][0][0])[0]
        case["observation"] = {
            "settings": document.get("Settings"),
            "planning_time": "Planning Time" in document,
            "execution_time": "Execution Time" in document,
        }
    return result


if __name__ == "__main__":
    print(json.dumps(observe(os.environ["PG_COMPLETION_CONNECTION"]), indent=2))
