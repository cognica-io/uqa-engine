#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture EXPLAIN input analysis with PostgreSQL 18's extended query protocol.

PG_PREPARED_CONNECTION must name an explicit disposable reference connection.
The temporary table and prepared statements belong only to that connection.
This fixture observes Parse errors and parameter types, not plan rendering.
"""

import ctypes as c
import ctypes.util
import json
import os


DECLARATION = "CREATE TEMP TABLE explain_inputs (id integer PRIMARY KEY, amount integer, label text, items integer[])"
CASES = [
    "EXPLAIN SELECT $1::integer + $2 AS total",
    "EXPLAIN SELECT $1 AS value",
    "EXPLAIN INSERT INTO explain_inputs (id, amount, items) VALUES ($1, $2, $3)",
    "EXPLAIN INSERT INTO explain_inputs (id, amount) SELECT $1, $2",
    "EXPLAIN UPDATE explain_inputs SET amount = $1 WHERE id = $2 RETURNING $3",
    "EXPLAIN UPDATE explain_inputs SET items[$1] = $2 WHERE id = $3",
    "EXPLAIN UPDATE explain_inputs SET items[$1:$2] = $3",
    "EXPLAIN DELETE FROM explain_inputs WHERE id = $1 RETURNING $2",
    "EXPLAIN MERGE INTO explain_inputs AS t USING (VALUES ($1::integer)) AS s(id) ON t.id = s.id WHEN MATCHED THEN UPDATE SET amount = $2 WHEN NOT MATCHED THEN INSERT (id, amount) VALUES (s.id, $3)",
    "EXPLAIN WITH changed AS (UPDATE explain_inputs SET amount = $1 RETURNING id) SELECT id FROM changed WHERE id = $2",
    "EXPLAIN WITH source AS (SELECT $1::integer AS amount) UPDATE explain_inputs SET amount = source.amount FROM source WHERE explain_inputs.id = $2",
    "EXPLAIN UPDATE explain_inputs SET amount = (SELECT $1::integer) WHERE id = $2",
    "EXPLAIN (ANALYZE, FORMAT JSON) INSERT INTO explain_inputs (id, amount) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET amount = $3",
    "EXPLAIN CREATE TABLE explain_output AS SELECT $1::integer AS value",
    "EXPLAIN DECLARE explain_cursor CURSOR FOR SELECT $1::integer AS value",
    "CREATE TEMP TABLE explain_control AS SELECT $1::integer AS value",
    "DECLARE explain_control_cursor CURSOR FOR SELECT $1::integer AS value",
    "EXPLAIN SELECT * FROM missing_explain_source WHERE id = $1",
    "EXPLAIN UPDATE missing_explain_target SET amount = 'bad'::integer",
    "EXPLAIN UPDATE explain_inputs SET missing_column = 'bad'::integer",
    "EXPLAIN UPDATE explain_inputs SET amount = 'bad'::integer WHERE false",
    "EXPLAIN INSERT INTO explain_inputs (id, amount) VALUES (1, 'bad')",
    "EXPLAIN DELETE FROM explain_inputs WHERE amount = 'bad'",
    "EXPLAIN UPDATE explain_inputs SET amount = $1 WHERE label = $1",
    "EXPLAIN UPDATE explain_inputs SET amount = $1 RETURNING $1",
    "EXPLAIN UPDATE explain_inputs SET amount = $1 WHERE false RETURNING 'bad'::integer",
    "EXPLAIN INSERT INTO explain_inputs (id, amount) VALUES ($1, $1::text)",
    "EXPLAIN MERGE INTO explain_inputs AS t USING (VALUES (1)) AS s(id) ON t.id = s.id WHEN MATCHED THEN UPDATE SET amount = 'bad'",
]


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
    status = function("PQresultStatus", integer, pointer)
    error = function("PQresultErrorField", string, pointer, integer)
    count = function("PQnparams", integer, pointer)
    parameter_type = function("PQparamtype", oid, pointer, integer)
    value = function("PQgetvalue", string, pointer, integer, integer)
    transaction_status = function("PQtransactionStatus", integer, pointer)

    def read(result):
        if not result:
            raise RuntimeError(connection_error(connection).decode())
        try:
            return {
                "status": status(result),
                "sqlstate": (error(result, ord("C")) or b"").decode(),
                "message": (error(result, ord("M")) or b"").decode(),
                "parameters": [parameter_type(result, index) for index in range(count(result))],
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
        result = query(connection, b"SELECT version()")
        try:
            version = value(result, 0, 0).decode()
        finally:
            clear(result)
        if not version.startswith("PostgreSQL 18."):
            raise RuntimeError(f"expected PostgreSQL 18, got {version!r}")
        setup = read(query(connection, DECLARATION.encode()))
        if setup["status"] != 1:
            raise RuntimeError(setup)
        cases = []
        for index, sql in enumerate(CASES):
            if read(query(connection, b"BEGIN"))["status"] != 1:
                raise RuntimeError("could not start reference transaction")
            name = f"explain_inputs_{index}".encode()
            observed = read(prepare(connection, name, sql.encode(), 0, None))
            if observed["status"] == 1:
                observed = read(describe(connection, name))
            cases.append({"sql": sql, "result": observed})
            if read(query(connection, b"ROLLBACK"))["status"] != 1:
                raise RuntimeError("could not roll back reference transaction")
        return {"postgresql_version": version, "declaration": DECLARATION, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_PREPARED_CONNECTION"]), indent=2))
