#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture table-function descriptors and correlated rows on disposable PostgreSQL 18.

The named container must provide Python 3 and libpq. Definitions are temporary
and the capture rolls back its transaction. No UQA executable supplies results.
"""

import argparse
import json
from pathlib import Path
import subprocess

CAPTURE = r'''
import ctypes as c, ctypes.util, json
pq = c.CDLL(ctypes.util.find_library("pq"))
def fn(name, result, *args):
    value = getattr(pq, name); value.restype = result; value.argtypes = args; return value
p, s, i = c.c_void_p, c.c_char_p, c.c_int
connect = fn("PQconnectdb", p, s)
status = fn("PQstatus", i, p)
execute = fn("PQexec", p, p, s)
clear = fn("PQclear", None, p)
finish = fn("PQfinish", None, p)
error = fn("PQresultErrorField", s, p, i)
height = fn("PQntuples", i, p)
width = fn("PQnfields", i, p)
name = fn("PQfname", s, p, i)
value = fn("PQgetvalue", s, p, i, i)
is_null = fn("PQgetisnull", i, p, i, i)
connection = connect(b"dbname=postgres user=postgres")
assert connection and status(connection) == 0, "reference connection failed"
def query(sql):
    result = execute(connection, sql.encode())
    assert result, "libpq returned no result"
    try:
        return {
            "columns": [name(result, col).decode() for col in range(width(result))],
            "rows": [[None if is_null(result, row, col) else value(result, row, col).decode()
                      for col in range(width(result))] for row in range(height(result))],
            "sqlstate": (error(result, ord("C")) or b"").decode() or None,
        }
    finally: clear(result)
try:
    version = query("SELECT version()")['rows'][0][0]
    assert version.startswith("PostgreSQL 18."), version
    assert query("BEGIN; " + fixture['setup'])['sqlstate'] is None
    results = []
    for case in fixture['cases']:
        assert query("SELECT setval('pg_temp.host_schema_calls',1,false); SAVEPOINT probe")['sqlstate'] is None
        output = query(case['query'])
        query("ROLLBACK TO probe")
        count = query("SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM pg_temp.host_schema_calls")['rows'][0][0]
        results.append(dict(case, result=output, calls=int(count)))
    print(json.dumps({'reference':version,'setup':fixture['setup'],'cases':results},indent=2))
finally:
    query("ROLLBACK"); finish(connection)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("container")
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    fixture = json.loads((directory / "host_table_schema.input.json").read_text())
    program = "fixture = " + repr(fixture) + "\n" + CAPTURE
    result = subprocess.run(
        ["docker", "exec", "-i", args.container, "python3", "-"],
        input=program, text=True, capture_output=True, check=True,
    )
    captured = json.loads(result.stdout)
    captured["image_id"] = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True,
    ).strip()
    destination = directory / "host_table_schema.expected.json"
    destination.write_text(json.dumps(captured, indent=2) + "\n")
    print(f"Captured {len(captured['cases'])} independent cases in {destination.name}")


if __name__ == "__main__":
    main()
