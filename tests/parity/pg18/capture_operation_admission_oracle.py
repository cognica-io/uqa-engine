#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture operation errors and savepoint recovery on a PostgreSQL 18 connection.

Set PG_ADMISSION_CONNECTION to an explicit reference connection. The sequence is
temporary and the ordinary table is created inside a rolled-back transaction.
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

    pointer, string, integer = c.c_void_p, c.c_char_p, c.c_int
    connect = function("PQconnectdb", pointer, string)
    connection_status = function("PQstatus", integer, pointer)
    connection_error = function("PQerrorMessage", string, pointer)
    finish = function("PQfinish", None, pointer)
    clear = function("PQclear", None, pointer)
    execute = function("PQexec", pointer, pointer, string)
    status = function("PQresultStatus", integer, pointer)
    error = function("PQresultErrorField", string, pointer, integer)
    transaction_status = function("PQtransactionStatus", integer, pointer)
    value = function("PQgetvalue", string, pointer, integer, integer)
    connection = connect(connection_string.encode())
    if not connection:
        raise RuntimeError("libpq could not allocate a connection")
    try:
        if connection_status(connection) != 0:
            raise RuntimeError(connection_error(connection).decode())

        def query(sql):
            result = execute(connection, sql.encode())
            if not result:
                raise RuntimeError(connection_error(connection).decode())
            try:
                return {
                    "sqlstate": (error(result, ord("C")) or b"").decode(),
                    "message": (error(result, ord("M")) or b"").decode(),
                    "transaction_status": transaction_status(connection),
                    "value": value(result, 0, 0).decode() if status(result) == 2 else None,
                }
            finally:
                clear(result)

        version = query("SELECT version()")["value"]
        if not version.startswith("PostgreSQL 18."):
            raise RuntimeError(f"expected PostgreSQL 18, got {version!r}")
        assert not query("CREATE TEMP SEQUENCE admission_ids MAXVALUE 2")["sqlstate"]
        cases = []
        for sql in ["SELECT lastval()", "SELECT currval('admission_ids')", "SELECT setval('admission_ids', 3)", "SELECT nextval('admission_ids')"]:
            assert not query("BEGIN")["sqlstate"]
            assert not query("SAVEPOINT keep")["sqlstate"]
            if sql.startswith("SELECT nextval"):
                assert query(sql)["value"] == "1"
                assert query(sql)["value"] == "2"
            failure = query(sql)
            assert failure["sqlstate"]
            blocked = query("SELECT setval('admission_ids', 1, false)")
            recovered = query("ROLLBACK TO keep")
            after_recovery = query("SELECT 1")
            rolled_back = query("ROLLBACK")
            cases.append({"sql": sql, "failure": failure, "blocked": blocked, "recovered": recovered, "after_recovery": after_recovery, "rolled_back": rolled_back})
        current_value = query("SELECT currval('admission_ids')")
        exhausted = query("SELECT nextval('admission_ids')")
        table = "admission_reference_" + query("SELECT pg_backend_pid()")["value"]
        assert not query("BEGIN")["sqlstate"]
        assert not query(f"CREATE TABLE {table} (n integer)")["sqlstate"]
        assert not query("SAVEPOINT keep")["sqlstate"]
        assert not query("SET TRANSACTION READ ONLY")["sqlstate"]
        read_only = {"failure": query(f"DELETE FROM {table}"), "blocked": query("SELECT 1"), "recovered": query("ROLLBACK TO keep"), "after_recovery": query("SELECT 1"), "rolled_back": query("ROLLBACK")}
        return {"postgresql_version": version, "cases": cases, "current_value": current_value, "exhausted": exhausted, "read_only": read_only}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_ADMISSION_CONNECTION"]), indent=2))
