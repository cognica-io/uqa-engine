#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture live I/O settings and temporary EXPLAIN fields from disposable PostgreSQL 18 using libpq; no UQA result supplies the reference."""

import ctypes as c
import ctypes.util
import hashlib
import json
from pathlib import Path
import subprocess
import sys


SETTINGS = [
    "SHOW track_io_timing",
    "SELECT name, setting, unit, category, short_desc, context, vartype, boot_val, reset_val FROM pg_settings WHERE name = 'track_io_timing'",
    "SET track_io_timing = on",
    "BEGIN",
    "SET LOCAL track_io_timing = off",
    "SAVEPOINT timing_setting",
    "SET track_io_timing = on",
    "ROLLBACK TO timing_setting",
    "COMMIT",
    "BEGIN",
    "SET track_io_timing = off",
    "ROLLBACK",
    "RESET track_io_timing",
    "SET track_io_timing = 'invalid'",
    "SET track_io_timing = 'yes'",
    "RESET ALL",
    "BEGIN",
    "CREATE ROLE uqa_io_timing_reader",
    "SET ROLE uqa_io_timing_reader",
    "SHOW track_io_timing",
    "SET track_io_timing = on",
    "ROLLBACK",
    "BEGIN",
    "SET track_io_timing = on",
    "CREATE FUNCTION scoped_io_timing() RETURNS text LANGUAGE SQL SET track_io_timing = off AS $$ SELECT current_setting('track_io_timing') $$",
    "SELECT scoped_io_timing(), current_setting('track_io_timing')",
    "CREATE FUNCTION failing_io_timing() RETURNS integer LANGUAGE SQL SET track_io_timing = off AS $$ SELECT 1 / 0 $$",
    "SAVEPOINT timing_failure",
    "SELECT failing_io_timing()",
    "ROLLBACK TO timing_failure",
    "SELECT set_config('track_io_timing', 'off', true)",
    "COMMIT",
    "SET track_io_timing = off",
    "DROP FUNCTION scoped_io_timing()",
    "DROP FUNCTION failing_io_timing()",
]

SPILL = "SELECT n FROM generate_series(1,4096) AS input(n) ORDER BY n DESC"


def capture():
    pq = c.CDLL(ctypes.util.find_library("pq"))

    def fn(name, result, *args):
        value = getattr(pq, name)
        value.restype, value.argtypes = result, args
        return value

    pointer, string, integer = c.c_void_p, c.c_char_p, c.c_int
    connect = fn("PQconnectdb", pointer, string)
    status = fn("PQstatus", integer, pointer)
    execute = fn("PQexec", pointer, pointer, string)
    clear = fn("PQclear", None, pointer)
    finish = fn("PQfinish", None, pointer)
    error = fn("PQresultErrorField", string, pointer, integer)
    height = fn("PQntuples", integer, pointer)
    width = fn("PQnfields", integer, pointer)
    value = fn("PQgetvalue", string, pointer, integer, integer)
    is_null = fn("PQgetisnull", integer, pointer, integer, integer)
    transaction = fn("PQtransactionStatus", integer, pointer)
    connection = connect(b"dbname=postgres user=postgres")
    assert connection and status(connection) == 0, "reference connection failed"

    def query(sql):
        result = execute(connection, sql.encode())
        assert result, "libpq returned no result"
        try:
            return {
                "sql": sql,
                "rows": [[None if is_null(result, row, col) else value(result, row, col).decode()
                          for col in range(width(result))] for row in range(height(result))],
                "sqlstate": (error(result, ord("C")) or b"").decode(),
                "message": (error(result, ord("M")) or b"").decode(),
                "transaction_status": transaction(connection),
            }
        finally:
            clear(result)

    def success(sql):
        result = query(sql)
        assert not result["sqlstate"], result
        return result["rows"]

    try:
        version = success("SELECT version()")[0][0]
        assert version.startswith("PostgreSQL 18."), version
        settings = []
        for sql in SETTINGS:
            result = query(sql)
            if result["transaction_status"] != 3:
                result["effective_timing"] = success("SHOW track_io_timing")[0][0] == "on"
            settings.append(result)
        success("SET work_mem = '64kB'")
        explanations = []
        for enabled in (False, True):
            for buffers in (False, True):
                for timing in (False, True):
                    for body in ("SELECT 1", SPILL):
                        setup = f"SET track_io_timing = {str(enabled).lower()}"
                        success(setup)
                        sql = f"EXPLAIN (ANALYZE, BUFFERS {str(buffers).lower()}, TIMING {str(timing).lower()}, SUMMARY FALSE, FORMAT JSON) {body}"
                        plan = json.loads(success(sql)[0][0])[0]["Plan"]
                        explanations.append({
                            "setup": setup, "sql": sql,
                            "read": None if "Temp Read Blocks" not in plan else plan["Temp Read Blocks"] > 0,
                            "written": None if "Temp Written Blocks" not in plan else plan["Temp Written Blocks"] > 0,
                            "io_time": "Temp I/O Read Time" in plan,
                            "operator_time": "Actual Total Time" in plan,
                        })
        success("BEGIN")
        success("CREATE FUNCTION change_io_timing(enabled text) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN PERFORM n FROM generate_series(1,4096) AS input(n) ORDER BY n DESC; PERFORM set_config('track_io_timing', enabled, false); RETURN 1; END $$")
        for before, after in ((True, False), (False, True)):
            success(f"SET track_io_timing = {str(before).lower()}")
            sql = f"EXPLAIN (ANALYZE, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) SELECT change_io_timing('{str(after).lower()}')"
            plan = json.loads(success(sql)[0][0])[0]["Plan"]
            explanations.append({
                "setup": f"SET track_io_timing = {str(before).lower()}", "sql": sql,
                "read": plan["Temp Read Blocks"] > 0,
                "written": plan["Temp Written Blocks"] > 0,
                "io_time": "Temp I/O Read Time" in plan,
                "operator_time": "Actual Total Time" in plan,
                "function": True,
            })
        partitioning = []

        def fields(value):
            if value is None:
                return None
            return {
                "read": None if "Temp Read Blocks" not in value else value["Temp Read Blocks"] > 0,
                "written": None if "Temp Written Blocks" not in value else value["Temp Written Blocks"] > 0,
                "io_time": "Temp I/O Read Time" in value,
            }

        def partition(sql, setup):
            success("DROP TABLE IF EXISTS io_destination")
            success(setup)
            document = json.loads(success(sql)[0][0])[0]
            partitioning.append({
                "setup": setup, "sql": sql,
                "planning": fields(document.get("Planning")),
                "plan": fields(document["Plan"]),
                "serialization": fields(document.get("Serialization")),
                "planning_time": "Planning Time" in document,
                "loops": document["Plan"].get("Actual Loops"),
            })

        for enabled in (False, True):
            for buffers in (False, True):
                setup = f"SET track_io_timing = {str(enabled).lower()}"
                for serialize in ("NONE", "TEXT", "BINARY"):
                    for body in (
                        SPILL,
                        f"CREATE TEMP TABLE io_destination AS {SPILL}",
                        f"CREATE TEMP TABLE io_destination AS {SPILL} WITH NO DATA",
                    ):
                        partition(f"EXPLAIN (ANALYZE, BUFFERS {str(buffers).lower()}, SERIALIZE {serialize}, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) {body}", setup)
                partition(f"EXPLAIN (BUFFERS {str(buffers).lower()}, SUMMARY FALSE, FORMAT JSON) SELECT 1", setup)
        for before, after in ((True, False), (False, True)):
            partition(f"EXPLAIN (ANALYZE, SERIALIZE TEXT, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) SELECT change_io_timing('{str(after).lower()}')", f"SET track_io_timing = {str(before).lower()}")
        success("PREPARE io_parameter(integer) AS SELECT $1")
        for before, after in ((True, False), (False, True)):
            partition(f"EXPLAIN (ANALYZE, SERIALIZE TEXT, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) EXECUTE io_parameter(change_io_timing('{str(after).lower()}'))", f"SET track_io_timing = {str(before).lower()}")
        partition("EXPLAIN (BUFFERS, SUMMARY FALSE, FORMAT JSON) EXECUTE io_parameter(change_io_timing('true'))", "SET track_io_timing = true")
        success("DEALLOCATE io_parameter")
        return {"provenance": {"version": version}, "settings": settings, "explanations": explanations, "partitioning": partitioning}
    finally:
        query("ROLLBACK")
        finish(connection)


def main():
    if sys.argv[1:] == ["--capture"]:
        print(json.dumps(capture()))
        return
    if len(sys.argv) != 2:
        raise SystemExit("usage: capture_io_timing.py <disposable PostgreSQL 18 container with Python/libpq>")
    source = Path(__file__).read_bytes()
    container = sys.argv[1]
    output = subprocess.check_output(["docker", "exec", "-i", container, "python3", "-", "--capture"], input=source)
    reference = json.loads(output)
    reference["provenance"].update({
        "collector_sha256": hashlib.sha256(source).hexdigest(),
        "image_id": subprocess.check_output(["docker", "inspect", "--format", "{{.Image}}", container], text=True).strip(),
    })
    destination = Path(__file__).with_name("io_timing.expected.json")
    destination.write_text(json.dumps(reference, indent=2) + "\n")
    print(f"Captured {len(reference['settings'])} setting operations, {len(reference['explanations'])} explanations and {len(reference['partitioning'])} partition cases in {destination.name}")


if __name__ == "__main__":
    main()
