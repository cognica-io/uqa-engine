#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture planning-memory field semantics from an explicit disposable PostgreSQL 18 connection; omit allocator-dependent totals and timing."""

import hashlib
import itertools
import json
import os
from pathlib import Path
import re
import xml.etree.ElementTree as ET

from capture_explain_options import capture


def memory_observation(rows, format_name):
    output = "\n".join(row[0] for row in rows)
    if format_name == "JSON":
        document = json.loads(output)
        assert isinstance(document, list) and len(document) == 1, output
        query = document[0]
        assert isinstance(query, (dict, str)), output
        planning = query.get("Planning") if isinstance(query, dict) else None
        used = None if planning is None else planning.get("Memory Used")
        allocated = None if planning is None else planning.get("Memory Allocated")
        has_planning = planning is not None
    elif format_name == "XML":
        planning = ET.fromstring(output).find(".//{*}Query/{*}Planning")
        used = None if planning is None else planning.findtext("{*}Memory-Used")
        allocated = None if planning is None else planning.findtext("{*}Memory-Allocated")
        has_planning = planning is not None
    elif format_name == "YAML":
        group = re.search(r"(?m)^  Planning:\s*$", output)
        values = re.search(r"(?m)^    Memory Used: (\d+)\n    Memory Allocated: (\d+)\s*$", output)
        used, allocated = values.groups() if values else (None, None)
        has_planning = group is not None
    else:
        values = re.search(r"(?m)^  Memory: used=(\d+)kB  allocated=(\d+)kB\s*$", output)
        used, allocated = values.groups() if values else (None, None)
        has_planning = "\nPlanning:" in output
    assert (used is None) == (allocated is None), output
    if used is not None:
        assert int(used) > 0 and int(allocated) >= int(used), output
    return {"planning": has_planning, "memory": used is not None}


def probes(record, cases):
    def sql(statement):
        record("sql", sql=statement)

    def explain(body, format_name, **settings):
        options = ", ".join(f"{name.upper()} {'TRUE' if enabled else 'FALSE'}" for name, enabled in settings.items())
        statement = f"EXPLAIN ({options}, FORMAT {format_name}) {body}"
        record("sql", keep_plan=True, sql=statement)
        case = cases[-1]
        case["format"] = format_name
        case["settings"] = settings
        result = case["result"]
        if result["status"] == 2:
            case["observation"] = memory_observation(result.pop("rows"), format_name)
        else:
            case["observation"] = None

    sql("CREATE TEMP SEQUENCE planning_memory_effect")
    for format_name, memory, analyze, summary, buffers in itertools.product(
        ("TEXT", "JSON", "XML", "YAML"), (False, True), (False, True), (False, True), (False, True)
    ):
        for timing in ((False, True) if analyze else (False,)):
            explain("SELECT nextval('planning_memory_effect')", format_name, memory=memory,
                    analyze=analyze, summary=summary, buffers=buffers, timing=timing)
            sql("SELECT last_value, is_called FROM planning_memory_effect")

    sql("CREATE TEMP TABLE planning_memory_input(v integer)")
    sql("INSERT INTO planning_memory_input VALUES (1), (2)")
    sql("PREPARE planning_memory_parameter(integer) AS SELECT v + $1 FROM planning_memory_input")
    for mode in ("force_generic_plan", "force_custom_plan", "auto"):
        sql(f"SET plan_cache_mode = {mode}")
        for argument in range(1, 8):
            explain(f"EXECUTE planning_memory_parameter({argument})", "JSON", memory=True,
                    analyze=argument % 2 == 0, summary=False, timing=False, buffers=False)
        sql("ALTER TABLE planning_memory_input ADD COLUMN other integer")
        explain("EXECUTE planning_memory_parameter(8)", "JSON", memory=True, summary=False)
        sql("ALTER TABLE planning_memory_input DROP COLUMN other")
    sql("DEALLOCATE planning_memory_parameter")

    for format_name, memory, analyze in itertools.product(("TEXT", "JSON", "XML", "YAML"), (False, True), (False, True)):
        for body in (
            "DECLARE planning_memory_cursor CURSOR FOR SELECT 1",
            "CREATE TEMP TABLE planning_memory_destination AS SELECT 1 AS v",
            "CREATE TEMP TABLE planning_memory_destination AS SELECT 1 AS v WITH NO DATA",
            "CREATE MATERIALIZED VIEW planning_memory_materialized AS SELECT 1 AS v",
        ):
            sql("BEGIN")
            explain(body, format_name, memory=memory, analyze=analyze, summary=False, timing=False, buffers=False)
            sql("ROLLBACK")
    sql("BEGIN")
    sql("CREATE TEMP TABLE planning_memory_exists(v integer)")
    for format_name in ("TEXT", "JSON", "XML", "YAML"):
        explain("CREATE TEMP TABLE IF NOT EXISTS planning_memory_exists AS SELECT 1 / 0 AS v", format_name,
                memory=True, analyze=True, summary=False, timing=False, buffers=False)
    sql("ROLLBACK")


if __name__ == "__main__":
    result = capture(os.environ["PG_EXPLAIN_CONNECTION"], probes)
    result["provenance"] = {
        "collector_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "libpq_helper_sha256": hashlib.sha256(Path(__file__).with_name("capture_explain_options.py").read_bytes()).hexdigest(),
        "image_id": os.environ["PG_REFERENCE_IMAGE_ID"],
        "memory_values": "Positive integer kilobytes with used <= allocated; allocator-dependent totals are deliberately omitted.",
    }
    print(json.dumps(result, indent=2))
