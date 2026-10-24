#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture prepared CALL inference, descriptors and invalidation through PostgreSQL 18 libpq.

Set PG_PREPARED_CONNECTION to an explicit disposable reference connection. The capture creates and drops the prepared_call_reference schema in a disposable database; prepared definitions and settings belong to this connection. Only Python's standard library and libpq are required.
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

        record("sql", sql="CREATE SCHEMA prepared_call_reference")
        record("sql", sql="SET search_path = prepared_call_reference")
        record("sql", sql="CREATE TABLE effects (value integer)")
        record("sql", sql="CREATE PROCEDURE change_value(IN amount integer, INOUT value integer, OUT label text) LANGUAGE plpgsql AS $$ BEGIN value := value + amount; label := 'changed'; INSERT INTO effects VALUES (value); END $$")
        record("sql", sql="CREATE PROCEDURE record_value(value integer DEFAULT 8) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO effects VALUES (value); END $$")
        record("sql", sql="CREATE PROCEDURE choose(IN value integer, OUT result integer) LANGUAGE plpgsql AS $$ BEGIN result := value + 1; END $$")
        record("sql", sql="CREATE PROCEDURE choose(IN value text, OUT result integer) LANGUAGE plpgsql AS $$ BEGIN result := 99; END $$")
        record("sql", sql="CREATE PROCEDURE read_date(IN value date, OUT result date) LANGUAGE plpgsql AS $$ BEGIN result := value; END $$")
        for case_name, sql, parameters in [
            ("inferred", "CALL change_value($1, $2, NULL)", ["3", "7"]),
            ("named", "CALL change_value(label => NULL, value => $1, amount => $2)", ["11", "2"]),
            ("output", "CALL change_value(1, 2, $1)", [None]),
            ("defaulted", "CALL record_value()", []),
            ("plain", "CALL record_value($1)", ["17"]),
            ("text_overload", "CALL choose('7', NULL)", []),
            ("integer_overload", "CALL choose($1::integer, NULL)", ["21"]),
            ("literal", "CALL change_value('2', '9', NULL)", []),
            ("date_input", "CALL read_date('01/02/2026', NULL)", []),
        ]:
            record("prepare", name=case_name, sql=sql, types=[])
            record("describe", name=case_name)
            record("execute", name=case_name, parameters=parameters)
        record("sql", sql="SELECT value FROM effects ORDER BY value")
        record("sql", sql="SET DateStyle = 'ISO, DMY'")
        record("sql", sql="DISCARD PLANS")
        record("describe", name="date_input")
        record("sql", sql="SELECT generic_plans, custom_plans FROM pg_prepared_statements WHERE statement = 'CALL read_date(''01/02/2026'', NULL)'")
        record("execute", name="date_input", parameters=[])
        record("sql", sql="SELECT generic_plans, custom_plans FROM pg_prepared_statements WHERE statement = 'CALL change_value($1, $2, NULL)'")
        for case_name, sql in [
            ("missing", "CALL missing_procedure($1)"),
            ("subquery", "CALL record_value((SELECT 1))"),
            ("set_argument", "CALL record_value(generate_series(1, 2))"),
            ("bad_literal", "CALL record_value('bad')"),
            ("conflicting", "CALL change_value($1, 1, $1)"),
        ]:
            record("prepare", name=case_name, sql=sql, types=[])
        record("sql", sql="CREATE OR REPLACE PROCEDURE record_value(value integer DEFAULT 8) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO effects VALUES (value + 100); END $$")
        record("execute", name="plain", parameters=["2"])
        record("sql", sql="SELECT value FROM effects ORDER BY value")
        record("sql", sql="BEGIN")
        record("sql", sql="SAVEPOINT retry")
        record("prepare", name="bad_transaction", sql="CALL record_value('bad')", types=[])
        record("sql", sql="ROLLBACK TO retry")
        record("execute", name="inferred", parameters=["1", "1"])
        record("sql", sql="ROLLBACK")
        record("sql", sql="DROP PROCEDURE change_value(integer, integer)")
        record("describe", name="inferred")
        record("execute", name="inferred", parameters=["1", "2"])
        record("sql", sql="DROP PROCEDURE record_value(integer)")
        record("describe", name="plain")
        record("execute", name="plain", parameters=["1"])
        record("sql", sql="DROP SCHEMA prepared_call_reference CASCADE")
        return {"postgresql_version": version, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_PREPARED_CONNECTION"]), indent=2))
