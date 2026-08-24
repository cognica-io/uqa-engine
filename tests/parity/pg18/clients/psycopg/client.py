#!/usr/bin/env python3

import json
import os

import psycopg
import psycopg_pool


def require_sqlstate(error: Exception, expected: str) -> None:
    actual = getattr(error, "sqlstate", None)
    if actual != expected:
        raise AssertionError(f"expected SQLSTATE {expected}, got {actual}: {error}")


dsn = os.environ["UQA_PG18_MATRIX_DSN"]
pool = psycopg_pool.ConnectionPool(
    conninfo=dsn,
    min_size=1,
    max_size=1,
    kwargs={"autocommit": True},
    open=True,
)
pool.wait()

with pool.connection() as connection:
    with connection.cursor(binary=True) as cursor:
        cursor.execute(
            "SELECT %s::int4 + 1 AS value",
            (41,),
            prepare=True,
        )
        assert cursor.fetchone() == (42,)
        cursor.execute(
            "SELECT %s::int4 + 1 AS value",
            (99,),
            prepare=True,
        )
        assert cursor.fetchone() == (100,)

    connection.execute("BEGIN")
    try:
        connection.execute("SELECT 1 / 0")
        raise AssertionError("division by zero unexpectedly succeeded")
    except psycopg.Error as error:
        require_sqlstate(error, "22012")
    try:
        connection.execute("SELECT 1")
        raise AssertionError("failed transaction unexpectedly accepted a query")
    except psycopg.Error as error:
        require_sqlstate(error, "25P02")
    connection.execute("ROLLBACK")

    connection.execute("CREATE TEMP TABLE matrix_copy (id int4, value text)")
    with connection.cursor().copy("COPY matrix_copy FROM STDIN") as copy:
        copy.write_row((1, "one"))
        copy.write_row((2, "two"))
    count = connection.execute("SELECT count(*)::int8 FROM matrix_copy").fetchone()
    assert count == (2,)
    with connection.cursor().copy("COPY matrix_copy TO STDOUT") as copy:
        copied = b"".join(bytes(chunk) for chunk in copy)
    assert copied == b"1\tone\n2\ttwo\n"

with pool.connection() as connection:
    assert connection.execute("SELECT 1").fetchone() == (1,)

pool.close()
print(
    json.dumps(
        {
            "driver": "psycopg",
            "psycopg": psycopg.__version__,
            "psycopg_pool": psycopg_pool.__version__,
            "operations": [
                "binary-bind-result",
                "prepared-reuse",
                "copy-in-out",
                "transaction-error-recovery",
                "pool-reuse",
            ],
        },
        sort_keys=True,
    )
)
