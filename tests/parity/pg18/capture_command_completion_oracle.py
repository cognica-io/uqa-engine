#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture every libpq result from the completion fixture in a disposable database.

Set PG_COMPLETION_CONNECTION to an explicit libpq connection string and pass
the checked-in fixture on stdin. The fixture creates and drops SQL objects.
Only Python's standard library and the PostgreSQL libpq shared library are used.
"""

import argparse
import ctypes
import ctypes.util
import json
import os
import sys


def capture(queries, connection_string, include_rows=False, include_fields=False, include_details=False, include_notices=False):
    library = ctypes.util.find_library("pq")
    if library is None:
        raise RuntimeError("PostgreSQL libpq shared library was not found")
    pq = ctypes.CDLL(library)

    def function(name, result_type, *argument_types):
        callback = getattr(pq, name)
        callback.restype = result_type
        callback.argtypes = argument_types
        return callback

    pointer = ctypes.c_void_p
    string = ctypes.c_char_p
    integer = ctypes.c_int
    connect = function("PQconnectdb", pointer, string)
    status = function("PQstatus", integer, pointer)
    error_message = function("PQerrorMessage", string, pointer)
    finish = function("PQfinish", None, pointer)
    send = function("PQsendQuery", integer, pointer, string)
    get = function("PQgetResult", pointer, pointer)
    result_status = function("PQresultStatus", integer, pointer)
    command = function("PQcmdStatus", string, pointer)
    field = function("PQresultErrorField", string, pointer, integer)
    value = function("PQgetvalue", string, pointer, integer, integer)
    clear = function("PQclear", None, pointer)
    nfields = function("PQnfields", integer, pointer)
    ntuples = function("PQntuples", integer, pointer)
    fname = function("PQfname", string, pointer, integer)
    ftype = function("PQftype", ctypes.c_uint, pointer, integer)
    fsize = function("PQfsize", integer, pointer, integer)
    fmod = function("PQfmod", integer, pointer, integer)
    ftable = function("PQftable", ctypes.c_uint, pointer, integer)
    fcolumn = function("PQftablecol", integer, pointer, integer)
    fformat = function("PQfformat", integer, pointer, integer)
    isnull = function("PQgetisnull", integer, pointer, integer, integer)
    notice_receiver = ctypes.CFUNCTYPE(None, pointer, pointer)
    set_notice_receiver = function("PQsetNoticeReceiver", pointer, pointer, notice_receiver, pointer)
    notices = []

    def diagnostic(result):
        """The severity, SQLSTATE, message, DETAIL and HINT of an error or notice."""
        fields = {}
        for key, letter in (("severity", "V"), ("sqlstate", "C"), ("message", "M"), ("detail", "D"), ("hint", "H")):
            text = field(result, ord(letter))
            fields[key] = None if text is None else text.decode()
        return fields

    receiver = notice_receiver(lambda _argument, result: notices.append(diagnostic(result)))
    connection = connect(connection_string.encode())
    if not connection:
        raise RuntimeError("libpq could not allocate a connection")
    try:
        if status(connection):
            raise RuntimeError(error_message(connection).decode())
        set_notice_receiver(connection, receiver, None)
        records = []
        version = None
        for sql in queries:
            notices.clear()
            if not send(connection, sql.encode()):
                raise RuntimeError(error_message(connection).decode())
            tags, error, results = [], None, []
            while True:
                result = get(connection)
                if not result:
                    break
                try:
                    code = result_status(result)
                    if code in (0, 1, 2):  # empty query, command, tuples
                        tags.append(command(result).decode() or None)
                        if include_rows or include_fields:
                            width = nfields(result)
                            row_result = {
                                "columns": [fname(result, column).decode() for column in range(width)],
                                "type_oids": [ftype(result, column) for column in range(width)],
                                "rows": [[None if isnull(result, row, column) else value(result, row, column).decode() for column in range(width)] for row in range(ntuples(result))],
                            }
                            if include_fields:
                                row_result["fields"] = [{
                                    "name": fname(result, column).decode(),
                                    "table_oid": ftable(result, column),
                                    "column_attribute_number": fcolumn(result, column),
                                    "type_oid": ftype(result, column),
                                    "type_size": fsize(result, column),
                                    "type_modifier": fmod(result, column),
                                    "format": fformat(result, column),
                                } for column in range(width)]
                            results.append(row_result)
                        if sql == "SELECT version()":
                            version = value(result, 0, 0).decode()
                            if not version.startswith("PostgreSQL 18."):
                                raise RuntimeError(f"expected a PostgreSQL 18 reference server, got {version!r}")
                    elif code in (6, 7):  # nonfatal or fatal SQL error
                        error = {
                            "sqlstate": field(result, ord("C")).decode(),
                            "message": field(result, ord("M")).decode(),
                        }
                        if include_details:
                            for key, code_letter in (("detail", "D"), ("hint", "H")):
                                text = field(result, ord(code_letter))
                                error[key] = None if text is None else text.decode()
                    else:
                        raise RuntimeError(f"unexpected libpq result status {code}")
                finally:
                    clear(result)
            record = {"sql": sql, "command_tags": tags, "error": error}
            if include_rows or include_fields:
                record["results"] = results
            if include_notices:
                record["notices"] = list(notices)
            records.append(record)
        if version is None or not version.startswith("PostgreSQL 18."):
            raise RuntimeError(f"expected a PostgreSQL 18 reference server, got {version!r}")
        return {"postgresql_version": version, "cases": records}
    finally:
        finish(connection)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rows", action="store_true", help="also capture field names, PostgreSQL type OIDs, and every result row")
    parser.add_argument("--fields", action="store_true", help="also capture full result field descriptors and rows")
    parser.add_argument("--details", action="store_true", help="also capture the DETAIL and HINT fields of every error")
    parser.add_argument("--notices", action="store_true", help="also capture the severity, SQLSTATE, message, DETAIL and HINT of every notice and warning")
    args = parser.parse_args()
    connection_string = os.environ.get("PG_COMPLETION_CONNECTION")
    if not connection_string:
        raise RuntimeError("set PG_COMPLETION_CONNECTION to an explicit disposable reference database")
    fixture = json.load(sys.stdin)
    queries = [case["sql"] for case in fixture["cases"]]
    if not queries or queries[0] != "SELECT version()":
        raise RuntimeError("the first fixture query must identify the reference with SELECT version()")
    result = capture(queries, connection_string, include_rows=args.rows, include_fields=args.fields, include_details=args.details, include_notices=args.notices)
    json.dump(result, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
