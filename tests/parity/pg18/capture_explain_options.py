#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture EXPLAIN metadata, ordered option diagnostics and side effects on PostgreSQL 18.

Set PG_EXPLAIN_CONNECTION to an explicit disposable connection. Temporary objects and session-owned prepared definitions are created; permanent relation probes are rolled back or explicitly dropped after commit. Plan payloads are omitted: the oracle observes result descriptors, command tags, errors, state and nontransactional sequence effects, not PostgreSQL-specific physical plans or timing measurements.
"""

import ctypes as c
import ctypes.util
import json
import os


def capture(connection_string, probes=None):
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

        def record(action, keep_plan=False, **arguments):
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
            observation = read(result)
            plan_output = not keep_plan and observation["status"] == 2 and observation["columns"] and observation["columns"][0][0] == "QUERY PLAN"
            if plan_output:
                if not observation["rows"]:
                    raise RuntimeError("EXPLAIN returned no output")
                observation["rows"] = []
            cases.append({"action": action, **arguments, "plan_output": bool(plan_output), "result": observation})

        if probes is not None:
            probes(record, cases)
            return {"postgresql_version": version, "cases": cases}

        record("sql", sql="CREATE TEMP SEQUENCE explain_effect")
        record("sql", sql="CREATE TEMP TABLE explain_items (id integer)")
        options = [
            "", "FORMAT TEXT", "FORMAT JSON", "FORMAT XML", "FORMAT YAML",
            "FORMAT 'JSON'", 'FORMAT "JSON"', "FORMAT bogus", "FORMAT 1",
            "FORMAT true", "FORMAT", "FORMAT xml, FORMAT json",
            "FORMAT bogus, FORMAT xml", "FORMAT xml, FORMAT bogus",
            "ANALYZE false", "ANALYZE 0", "ANALYZE 1", "ANALYZE 2",
            "ANALYZE -1", "ANALYZE 1.5", "ANALYZE 'yes'", "ANALYZE 'no'",
            "ANALYZE 'on'", "ANALYZE 'off'", "ANALYZE '1'", "ANALYZE '0'", "ANALYZE 't'", "ANALYZE 'y'",
            "ANALYZE 'o'", "ANALYZE 'TRUE'", "ANALYZE ' true '",
            "ANALYZE bogus", "VERBOSE bogus", "unknown true", '"ANALYZE" true', '"FORMAT" xml',
            "ANALYZE false, ANALYZE true", "ANALYZE true, ANALYZE false",
            "COSTS false", "SETTINGS false", "GENERIC_PLAN false",
            "BUFFERS false", "WAL false", "TIMING false", "SUMMARY false",
            "MEMORY false", "SERIALIZE NONE", "SERIALIZE OFF",
            "WAL true", "TIMING true", "SERIALIZE", "SERIALIZE true",
            "SERIALIZE binary", "SERIALIZE 'TEXT'",
            "ANALYZE, GENERIC_PLAN", "WAL, TIMING, SERIALIZE",
            "ANALYZE true, FORMAT bogus", "ANALYZE bogus, FORMAT bogus",
            "FORMAT bogus, ANALYZE bogus", "unknown, FORMAT",
        ]
        for index, option in enumerate(options):
            prefix = "EXPLAIN " + ("(" + option + ") " if option else "")
            sql = prefix + "SELECT nextval('explain_effect')"
            prepared_name = f"explain_{index}"
            record("prepare", name=prepared_name, sql=sql, types=[])
            prepared = cases[-1]["result"]["status"] == 1
            if prepared:
                record("describe", name=prepared_name)
                record("execute", name=prepared_name, parameters=[])
            record("sql", sql="SELECT last_value, is_called FROM explain_effect")
        for format_name in ("text", "json", "xml", "yaml"):
            record("sql", sql="CREATE OR REPLACE FUNCTION pg_temp.explain_format_type() RETURNS text LANGUAGE plpgsql AS $$DECLARE r record; BEGIN FOR r IN EXPLAIN (FORMAT " + format_name + ") SELECT 1 LOOP RETURN pg_typeof(r.\"QUERY PLAN\")::text; END LOOP; RETURN 'missing'; END$$")
            record("sql", sql="SELECT pg_temp.explain_format_type()")
        for sql in [
            "EXPLAIN (FORMAT bogus) SELECT missing_column",
            "EXPLAIN (ANALYZE bogus) SELECT missing_column",
            "EXPLAIN (FORMAT) SELECT missing_column",
            "EXPLAIN (FORMAT bogus) SELECT 1 / 0",
            "EXPLAIN (ANALYZE, FORMAT bogus) INSERT INTO explain_items VALUES (1)",
            "SELECT count(*) FROM explain_items",
            "BEGIN", "SAVEPOINT explain_retry",
            "EXPLAIN (ANALYZE, FORMAT bogus) INSERT INTO explain_items VALUES (2)",
            "SELECT count(*) FROM explain_items", "ROLLBACK TO explain_retry",
            "SELECT count(*) FROM explain_items", "ROLLBACK",
            "BEGIN READ ONLY", "SAVEPOINT explain_read_only",
            "EXPLAIN (ANALYZE, FORMAT bogus) CREATE TEMP TABLE explain_created AS SELECT 1",
            "ROLLBACK TO explain_read_only",
            "EXPLAIN ANALYZE CREATE TEMP TABLE explain_created AS SELECT 1",
            "ROLLBACK TO explain_read_only",
            "EXPLAIN CREATE TEMP TABLE explain_created AS SELECT 1",
            "EXPLAIN (ANALYZE, FORMAT bogus) CREATE TABLE explain_permanent AS SELECT 1",
            "ROLLBACK TO explain_read_only",
            "EXPLAIN ANALYZE CREATE TABLE explain_permanent AS SELECT 1",
            "ROLLBACK TO explain_read_only", "ROLLBACK",
        ]:
            record("sql", sql=sql)
        for sql in [
            "CREATE FUNCTION pg_temp.explain_planning_effect() RETURNS bigint LANGUAGE SQL IMMUTABLE AS $$SELECT nextval('explain_effect')$$",
            "BEGIN",
            "EXPLAIN ANALYZE CREATE TEMP TABLE explained_planned_once AS SELECT pg_temp.explain_planning_effect() AS id",
            "SELECT * FROM explained_planned_once",
            "SELECT last_value, is_called FROM explain_effect", "ROLLBACK",
            "BEGIN",
            "EXPLAIN ANALYZE CREATE MATERIALIZED VIEW explained_materialized_once AS SELECT pg_temp.explain_planning_effect() AS id",
            "SELECT * FROM explained_materialized_once",
            "SELECT last_value, is_called FROM explain_effect",
            "REFRESH MATERIALIZED VIEW explained_materialized_once",
            "SELECT * FROM explained_materialized_once",
            "SELECT last_value, is_called FROM explain_effect", "ROLLBACK",
            "EXPLAIN ANALYZE DECLARE explained_cursor CURSOR FOR SELECT nextval('explain_effect')",
            "SELECT last_value, is_called FROM explain_effect",
            "SELECT count(*) FROM pg_cursors WHERE name = 'explained_cursor'",
            "PREPARE explained_execute(integer) AS SELECT nextval('explain_effect') + $1 AS value",
            "EXPLAIN EXECUTE explained_execute(2)",
            "SELECT last_value, is_called FROM explain_effect",
            "EXPLAIN ANALYZE EXECUTE explained_execute(3)",
            "SELECT last_value, is_called FROM explain_effect",
            "DEALLOCATE explained_execute",
            "BEGIN", "SAVEPOINT destination",
            "EXPLAIN CREATE TEMP TABLE explain_items AS SELECT 1/0",
            "ROLLBACK TO destination",
            "EXPLAIN ANALYZE CREATE TEMP TABLE explain_no_data AS SELECT nextval('explain_effect') AS id WITH NO DATA",
            "SELECT count(*) FROM explain_no_data",
            "SELECT last_value, is_called FROM explain_effect", "ROLLBACK",
            "BEGIN READ ONLY", "SAVEPOINT committed_destination",
            "EXPLAIN ANALYZE CREATE TABLE explain_committed AS SELECT 1 AS id",
            "SHOW transaction_read_only",
            "SAVEPOINT forbidden_insert", "INSERT INTO explain_committed VALUES (2)",
            "ROLLBACK TO forbidden_insert", "SELECT count(*) FROM explain_committed",
            "SHOW transaction_read_only", "COMMIT",
            "SELECT * FROM explain_committed", "DROP TABLE explain_committed",
            "BEGIN READ ONLY", "SAVEPOINT materialized",
            "EXPLAIN ANALYZE CREATE MATERIALIZED VIEW explain_materialized AS SELECT 1 AS id",
            "SELECT * FROM explain_materialized", "ROLLBACK",
        ]:
            record("sql", sql=sql)
        for format_name in ("text", "json", "xml", "yaml"):
            record("sql", keep_plan=True, sql="EXPLAIN (ANALYZE, FORMAT " + format_name + ") CREATE TEMP TABLE IF NOT EXISTS explain_items AS SELECT 1/0")
        for sql in [
            "CREATE FUNCTION pg_temp.explain_temporary() RETURNS integer LANGUAGE SQL AS 'SELECT 7'",
            "SELECT pg_temp.explain_temporary() AS value",
            "BEGIN", "SAVEPOINT temporary_function",
            "CREATE OR REPLACE FUNCTION pg_temp.explain_temporary() RETURNS integer LANGUAGE SQL AS 'SELECT 9'",
            "SELECT pg_temp.explain_temporary() AS value",
            "ROLLBACK TO temporary_function", "COMMIT",
            "SELECT pg_temp.explain_temporary() AS value",
            "SET search_path = pg_temp, public",
            "SELECT explain_temporary() AS value",
            "SELECT pg_temp.explain_temporary() AS value", "RESET search_path",
            "BEGIN", "DISCARD TEMP",
            "SELECT pg_temp.explain_temporary() AS value", "ROLLBACK",
            "SELECT pg_temp.explain_temporary() AS value",
            "DISCARD TEMP", "SELECT pg_temp.explain_temporary() AS value",
        ]:
            record("sql", sql=sql)
        return {"postgresql_version": version, "cases": cases}
    finally:
        finish(connection)


if __name__ == "__main__":
    print(json.dumps(capture(os.environ["PG_EXPLAIN_CONNECTION"]), indent=2))
