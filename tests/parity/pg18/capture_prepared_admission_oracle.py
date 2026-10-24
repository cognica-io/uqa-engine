#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture prepared operation admission and recovery through libpq on PostgreSQL 18.

Set PG_PREPARED_CONNECTION to an explicit disposable reference connection. The capture creates and drops one uniquely named domain in a disposable database; prepared definitions and settings belong to this connection. Only Python's standard library and libpq are required.
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
                types = [int(read(query(connection, ("SELECT '" + item.replace("'", "''") + "'::regtype::oid").encode()))["rows"][0][0]) if isinstance(item, str) else item for item in arguments["types"]]
                result = prepare(connection, arguments["name"].encode(), arguments["sql"].encode(), len(types), (oid * len(types))(*types))
            elif action == "describe":
                result = describe(connection, arguments["name"].encode())
            else:
                parameters = [None if item is None else item.encode() for item in arguments["parameters"]]
                result = execute(connection, arguments["name"].encode(), len(parameters), (string * len(parameters))(*parameters), None, None, 0)
            cases.append({"action": action, **arguments, "result": read(result)})

        record("sql", sql="CREATE DOMAIN prepared_admission_positive AS integer CHECK (VALUE > 0)")
        record("prepare", name="value", sql="SELECT $1::integer AS value", types=[])
        record("prepare", name="empty", sql="", types=[23])
        record("prepare", name="setting", sql="SET work_mem = '4MB'", types=[23])
        record("prepare", name="rollback", sql="ROLLBACK", types=[])
        record("prepare", name="typed_rollback", sql="ROLLBACK", types=[23])
        record("prepare", name="domain_rollback", sql="ROLLBACK", types=["prepared_admission_positive"])
        record("execute", name="domain_rollback", parameters=["0"])
        record("execute", name="domain_rollback", parameters=["1"])
        record("sql", sql="BEGIN")
        record("execute", name="domain_rollback", parameters=["0"])
        record("execute", name="rollback", parameters=[])
        record("sql", sql="BEGIN")
        record("sql", sql="SAVEPOINT retry")
        record("execute", name="value", parameters=[])
        record("describe", name="value")
        record("describe", name="setting")
        record("describe", name="empty")
        record("prepare", name="failed_empty", sql="", types=[])
        record("prepare", name="failed_rollback", sql="ROLLBACK TO SAVEPOINT retry", types=[])
        record("prepare", name="failed_commit", sql="COMMIT", types=[])
        record("prepare", name="failed_begin", sql="BEGIN", types=[])
        record("prepare", name="failed_show", sql="SHOW work_mem", types=[])
        record("execute", name="value", parameters=["1", "2"])
        record("execute", name="value", parameters=["1"])
        record("execute", name="empty", parameters=["1"])
        record("execute", name="typed_rollback", parameters=["1"])
        record("execute", name="failed_rollback", parameters=[])
        record("execute", name="value", parameters=["7"])
        record("describe", name="value")
        record("execute", name="setting", parameters=[])
        record("execute", name="failed_commit", parameters=[])
        record("sql", sql="BEGIN")
        record("execute", name="empty", parameters=[])
        record("execute", name="rollback", parameters=[])
        record("sql", sql="BEGIN")
        record("prepare", name="syntax", sql="SELECT FROM", types=[])
        record("prepare", name="syntax_failed", sql="SELECT FROM", types=[])
        record("prepare", name="recovery", sql="ROLLBACK", types=[])
        record("execute", name="recovery", parameters=[])
        record("sql", sql="BEGIN")
        record("sql", sql="DEALLOCATE ALL")
        record("describe", name="value")
        record("execute", name="value", parameters=[])
        record("sql", sql="ROLLBACK")
        record("sql", sql="DROP DOMAIN prepared_admission_positive")
        return {"postgresql_version": version, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_PREPARED_CONNECTION"]), indent=2))
