#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture COSTS visibility and SQL fixed-width estimates from an independent PostgreSQL 18 instance."""

import json
import os
import re
import subprocess
import xml.etree.ElementTree as xml


def capture():
    command = json.loads(os.environ["PG_EXPLAIN_COMMAND"])

    def query(sql):
        return subprocess.check_output([*command, "-XAt", "-v", "ON_ERROR_STOP=1", "-c", sql], text=True).strip()

    version = query("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    for body in ("SELECT 1::smallint, 2::integer, 3::bigint, true", "VALUES (1, 2::bigint), (3, 4::bigint)"):
        for options in ("", "COSTS", "COSTS FALSE", "COSTS FALSE, COSTS TRUE", "COSTS TRUE, COSTS FALSE"):
            for format_name in ("TEXT", "JSON", "XML", "YAML"):
                sql = f"EXPLAIN ({options + ', ' if options else ''}FORMAT {format_name}) {body}"
                text = query(sql)
                if format_name == "JSON":
                    plan = json.loads(text)[0]["Plan"]
                    visible = "Startup Cost" in plan
                    rows, width = plan.get("Plan Rows"), plan.get("Plan Width")
                elif format_name == "TEXT":
                    match = re.search(r"cost=([\d.]+)\.\.([\d.]+) rows=(\d+) width=(\d+)", text)
                    visible = match is not None
                    rows, width = (int(match[3]), int(match[4])) if match else (None, None)
                elif format_name == "XML":
                    root = xml.fromstring(text)
                    visible = root.find(".//{*}Startup-Cost") is not None
                    rows = int(root.find(".//{*}Plan-Rows").text) if visible else None
                    width = int(root.find(".//{*}Plan-Width").text) if visible else None
                else:
                    visible = "Startup Cost:" in text
                    rows = int(re.search(r"Plan Rows: (\d+)", text)[1]) if visible else None
                    width = int(re.search(r"Plan Width: (\d+)", text)[1]) if visible else None
                cases.append({"sql": sql, "format": format_name, "costs": visible, "rows": rows, "width": width})
    return {"postgresql_version": version, "cases": cases}


if __name__ == "__main__":
    print(json.dumps(capture(), indent=2))
