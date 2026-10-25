#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture independent PostgreSQL binary send outputs through a caller-supplied psql."""

import json
import os
import subprocess
from pathlib import Path


CASES = [
    ("smallint", "-32768"), ("integer", "2147483647"), ("bigint", "-9223372036854775808"),
    ("oid", "4294967295"), ("xid", "4294967295"), ("boolean", "true"), ("boolean", "false"),
    ("real", "1.25"), ("real", "-Infinity"), ("real", "NaN"), ("double precision", "NaN"), ("double precision", "-0"),
    ("double precision", "Infinity"), ("numeric", "0.0000"), ("numeric", "-12.340000"),
    ("numeric", "123456789012345678901234567890.0000000001"), ("numeric", "0.0000000123"),
    ("numeric", "100000000000000000"), ("numeric", "NaN"), ("numeric", "Infinity"),
    ("numeric", "-Infinity"), ("text", "한글\t\nabc"), ("char(4)", "a"), ("name", "한글"),
    ("uuid", "01234567-89ab-cdef-0123-456789abcdef"), ("bytea", r"\x00ff01"),
    ("bit(11)", "10101010101"), ("varbit", ""), ("varbit", "101"),
    ("date", "2000-01-01"), ("date", "1969-12-31"), ("date", "infinity"),
    ("date", "-infinity"), ("time", "23:59:59.123456"), ("timetz", "01:02:03.456789+05:30"),
    ("timestamp", "1999-12-31 23:59:59.999999"), ("timestamptz", "2000-01-01 00:00:00+02"),
    ("timestamp", "infinity"), ("timestamp", "-infinity"),
    ("interval", "2 mons -3 days 01:02:03.456789"), ("json", ' { "a": 1 } '),
    ("jsonb", '{"b": [true, null], "a": 1}'), ("xml", "<root>é</root>"),
    ("integer[]", "[0:1][3:4]={{1,NULL},{-2,4}}"), ("integer[]", "{}"),
    ("numeric[]", "{1.20,NULL,NaN,-Infinity}"), ("text[]", '{"a,b",NULL,""}'),
    ("int2vector", "1 -2 32767"), ("oidvector", "1 4294967295"),
    ("int4range", "[1,9)"), ("int4range", "empty"), ("int8range", "(,9)"),
    ("numrange", "(1.25,2.50]"), ("daterange", "[2000-01-01,infinity)"),
    ("tsrange", "[1999-12-31 23:59:59,2000-01-01)"), ("int4multirange", "{[1,3),[7,9)}"),
    ("nummultirange", "{}"), ("void", ""), ('"char"', "A"), ("refcursor", "portal"),
    ("regclass", "42"), ("regtype", "42"), ("regnamespace", "42"), ("regrole", "42"),
    ("regproc", "42"), ("regprocedure", "42"), ("regcollation", "42"),
]


def main():
    command = json.loads(os.environ["PG_EXPLAIN_COMMAND"])

    def sql(statement):
        return subprocess.check_output(command + ["-XAt", "-v", "ON_ERROR_STOP=1", "-c", statement], text=True).strip()

    version = sql("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    def binary_field(expression):
        wire = subprocess.check_output(command + ["-XAtq", "-v", "ON_ERROR_STOP=1", "-c", f"COPY (SELECT {expression}) TO STDOUT (FORMAT BINARY)"])
        if wire[:11] != b"PGCOPY\n\xff\r\n\x00":
            raise RuntimeError("invalid binary COPY signature")
        offset = 19 + int.from_bytes(wire[15:19], "big")
        if int.from_bytes(wire[offset:offset + 2], "big") != 1:
            raise RuntimeError("expected one binary output field")
        size = int.from_bytes(wire[offset + 2:offset + 6], "big", signed=True)
        if size < 0 or wire[offset + 6 + size:] != b"\xff\xff":
            raise RuntimeError("invalid binary field length or trailer")
        return wire[offset + 6:offset + 6 + size].hex()

    cases = []
    for ty, value in CASES:
        literal = "'" + value.replace("'", "''") + "'"
        type_oid = sql(f"SELECT pg_typeof(NULL::{ty})::oid")
        send = sql(f"SELECT typsend::regproc FROM pg_type WHERE oid = {type_oid}")
        expression = f"{literal}::{ty}"
        binary = binary_field(expression)
        cases.append({"type": ty, "input": value, "expression": expression, "send": send, "hex": binary})
    nested_records = []
    for depth in [1, 128, 256]:
        expression = "ROW(" * depth + "7::integer" + ")" * depth
        nested_records.append({"depth": depth, "hex": binary_field(expression)})
    path = Path(__file__).resolve().parents[3] / "crates/uqa-sql/src/result/binary/postgresql.json"
    path.write_text(json.dumps({"version": version, "method": "binary COPY invokes each selected pg_type.typsend; retain field bytes without COPY or DataRow framing", "cases": cases, "nested_records": nested_records}, ensure_ascii=False, indent=2) + "\n")
    print(f"Captured {len(cases)} binary result fields from {version}")


if __name__ == "__main__":
    main()
