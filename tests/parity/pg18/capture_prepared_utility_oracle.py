#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture session utility preparation and execution through libpq on PostgreSQL 18.

Set PG_PREPARED_CONNECTION to an explicit disposable reference connection. All prepared definitions and settings belong to this connection; no durable objects are created. Only Python's standard library and libpq are required.
"""

import ctypes as c
import ctypes.util
import json
import os


def capture(connection_string):
    pq = c.CDLL(ctypes.util.find_library("pq"))

    def function(name, result, *arguments):
        callback = getattr(pq, name)
        callback.restype = result
        callback.argtypes = arguments
        return callback

    pointer, string, integer, oid = c.c_void_p, c.c_char_p, c.c_int, c.c_uint
    connect = function("PQconnectdb", pointer, string)
    connection_status = function("PQstatus", integer, pointer)
    connection_error = function("PQerrorMessage", string, pointer)
    finish = function("PQfinish", None, pointer)
    clear = function("PQclear", None, pointer)
    query = function("PQexec", pointer, pointer, string)
    prepare = function("PQprepare", pointer, pointer, string, string, integer, c.POINTER(oid))
    describe = function("PQdescribePrepared", pointer, pointer, string)
    execute = function("PQexecPrepared", pointer, pointer, string, integer, c.POINTER(string), c.POINTER(integer), c.POINTER(integer), integer)
    status = function("PQresultStatus", integer, pointer)
    error = function("PQresultErrorField", string, pointer, integer)
    tag = function("PQcmdStatus", string, pointer)
    width = function("PQnfields", integer, pointer)
    height = function("PQntuples", integer, pointer)
    name = function("PQfname", string, pointer, integer)
    field_type = function("PQftype", oid, pointer, integer)
    count = function("PQnparams", integer, pointer)
    parameter_type = function("PQparamtype", oid, pointer, integer)
    value = function("PQgetvalue", string, pointer, integer, integer)
    is_null = function("PQgetisnull", integer, pointer, integer, integer)
    transaction_status = function("PQtransactionStatus", integer, pointer)

    def read(result):
        if not result:
            raise RuntimeError(connection_error(connection).decode())
        try:
            return {
                "status": status(result),
                "sqlstate": (error(result, ord("C")) or b"").decode(),
                "message": (error(result, ord("M")) or b"").decode(),
                "tag": tag(result).decode(),
                "columns": [[name(result, column).decode(), field_type(result, column)] for column in range(width(result))],
                "parameters": [parameter_type(result, index) for index in range(count(result))],
                "rows": [[None if is_null(result, row, column) else value(result, row, column).decode() for column in range(width(result))] for row in range(height(result))],
                "transaction_status": transaction_status(connection),
            }
        finally:
            clear(result)

    connection = connect(connection_string.encode())
    if not connection:
        raise RuntimeError("libpq could not allocate a connection")
    try:
        if connection_status(connection) != 0:
            raise RuntimeError(connection_error(connection).decode())
        version = read(query(connection, b"SELECT version()"))["rows"][0][0]
        if not version.startswith("PostgreSQL 18."):
            raise RuntimeError(f"expected PostgreSQL 18, got {version!r}")
        cases = []

        def record(action, **arguments):
            if action == "sql":
                result = query(connection, arguments["sql"].encode())
            elif action == "prepare":
                types = arguments["types"]
                result = prepare(connection, arguments["name"].encode(), arguments["sql"].encode(), len(types), (oid * len(types))(*types))
            elif action == "describe":
                result = describe(connection, arguments["name"].encode())
            else:
                parameters = [None if item is None else item.encode() for item in arguments["parameters"]]
                result = execute(connection, arguments["name"].encode(), len(parameters), (string * len(parameters))(*parameters), None, None, 0)
            cases.append({"action": action, **arguments, "result": read(result)})

        record("sql", sql="SET TIME ZONE 'UTC'")
        record("prepare", name="timezone", sql="SHOW TIME ZONE", types=[])
        record("describe", name="timezone")
        record("execute", name="timezone", parameters=[])
        record("prepare", name="settings", sql="SHOW ALL", types=[])
        record("describe", name="settings")
        record("sql", sql="SET work_mem = '4MB'")
        record("sql", sql="SET plan_cache_mode = force_custom_plan")
        record("prepare", name="typed_show", sql="SHOW work_mem", types=[23])
        record("describe", name="typed_show")
        record("execute", name="typed_show", parameters=["4"])
        record("sql", sql="SELECT generic_plans,custom_plans FROM pg_prepared_statements WHERE statement='SHOW work_mem'")
        record("sql", sql="BEGIN")
        record("prepare", name="snapshot_show", sql="SHOW work_mem", types=[])
        record("prepare", name="snapshot_fetch", sql="FETCH ALL FROM absent", types=[])
        record("sql", sql="SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        record("sql", sql="ROLLBACK")
        record("sql", sql="BEGIN")
        record("sql", sql="DECLARE c CURSOR FOR SELECT 1::integer AS old")
        record("prepare", name="fetch", sql="FETCH ALL FROM c", types=[])
        record("describe", name="fetch")
        record("sql", sql="CLOSE c")
        record("sql", sql="DECLARE c CURSOR FOR SELECT 'new'::text AS different")
        record("execute", name="fetch", parameters=[])
        record("describe", name="fetch")
        record("sql", sql="DISCARD PLANS")
        record("execute", name="fetch", parameters=[])
        record("describe", name="fetch")
        record("prepare", name="move", sql="MOVE ALL FROM c", types=[])
        record("describe", name="move")
        record("execute", name="move", parameters=[])
        record("prepare", name="late_fetch", sql="FETCH ALL FROM late", types=[])
        record("describe", name="late_fetch")
        record("sql", sql="DECLARE late CURSOR FOR SELECT 9::integer AS number")
        record("execute", name="late_fetch", parameters=[])
        record("describe", name="late_fetch")
        record("sql", sql="COMMIT")
        record("sql", sql="PREPARE inner_query AS SELECT 1::integer AS old")
        record("prepare", name="execute", sql="EXECUTE inner_query", types=[])
        record("describe", name="execute")
        record("execute", name="execute", parameters=[])
        record("sql", sql="DEALLOCATE inner_query")
        record("sql", sql="PREPARE inner_query AS SELECT 'new'::text AS different")
        record("execute", name="execute", parameters=[])
        record("describe", name="execute")
        record("sql", sql="DEALLOCATE inner_query")
        record("prepare", name="late_execute", sql="EXECUTE absent_query", types=[])
        record("describe", name="late_execute")
        record("execute", name="late_execute", parameters=[])
        record("sql", sql="PREPARE parameter_query(integer) AS SELECT $1 AS value")
        record("prepare", name="raw_execute", sql="EXECUTE parameter_query($1)", types=[])
        record("describe", name="raw_execute")
        record("execute", name="raw_execute", parameters=[])
        record("prepare", name="typed_execute", sql="EXECUTE parameter_query($1)", types=[23])
        record("describe", name="typed_execute")
        record("execute", name="typed_execute", parameters=["4"])
        record("prepare", name="literal_execute", sql="EXECUTE parameter_query(4)", types=[23])
        record("describe", name="literal_execute")
        record("execute", name="literal_execute", parameters=["5"])
        record("sql", sql="DEALLOCATE parameter_query")
        record("prepare", name="missing_show", sql="SHOW missing_prepared_setting", types=[])
        return {"postgresql_version": version, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_PREPARED_CONNECTION"]), indent=2))
