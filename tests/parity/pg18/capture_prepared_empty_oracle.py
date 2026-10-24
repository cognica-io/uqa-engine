#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture empty prepared-message behavior through libpq on PostgreSQL 18.

Set PG_PREPARED_CONNECTION to an explicit disposable reference connection.
All prepared definitions and settings belong to this connection; no durable
objects are created. Only Python's standard library and libpq are required.
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

        record("prepare", name="empty", sql="", types=[])
        record("describe", name="empty")
        record("execute", name="empty", parameters=[])
        record("sql", sql="SET plan_cache_mode=force_custom_plan")
        record("prepare", name="typed_empty", sql="", types=[23])
        record("describe", name="typed_empty")
        record("execute", name="typed_empty", parameters=["4"])
        record("sql", sql="SELECT generic_plans,custom_plans FROM pg_prepared_statements WHERE cardinality(parameter_types)=1")
        record("sql", sql="BEGIN")
        record("prepare", name="comments", sql=" ; -- empty\n; ", types=[])
        record("execute", name="comments", parameters=[])
        record("sql", sql="SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        record("sql", sql="SAVEPOINT keep")
        record("sql", sql="SELECT 1/0")
        record("prepare", name="failed_empty", sql="", types=[])
        record("describe", name="failed_empty")
        record("execute", name="failed_empty", parameters=[])
        record("execute", name="empty", parameters=[])
        record("sql", sql="")
        record("prepare", name="failed_missing", sql="SELECT * FROM missing_prepared_relation", types=[])
        record("prepare", name="failed_multi", sql="SELECT 1; SELECT 2", types=[])
        record("sql", sql="ROLLBACK TO keep")
        record("execute", name="failed_empty", parameters=[])
        record("sql", sql="COMMIT")
        return {"postgresql_version": version, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_PREPARED_CONNECTION"]), indent=2))
