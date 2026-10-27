#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture temporary namespace dependency cleanup and rollback from PostgreSQL 18."""

import json
import os
import re
import subprocess
import time
from pathlib import Path

SETUP = """
CREATE SCHEMA lifetime_test;
CREATE DOMAIN pg_temp.lifetime_domain AS integer CHECK(VALUE > 0);
CREATE TYPE pg_temp.lifetime_enum AS ENUM ('ready');
CREATE TYPE pg_temp.lifetime_composite AS (n integer);
CREATE TYPE pg_temp.lifetime_shell;
CREATE TABLE lifetime_test.dependent (n integer, d pg_temp.lifetime_domain, e pg_temp.lifetime_enum, c pg_temp.lifetime_composite);
INSERT INTO lifetime_test.dependent (n,d) VALUES(7,1);
CREATE VIEW lifetime_test.dependent_view AS SELECT d FROM lifetime_test.dependent;
CREATE FUNCTION lifetime_test.dependent_routine(pg_temp.lifetime_domain) RETURNS integer LANGUAGE SQL AS 'SELECT $1::integer';
"""
OBSERVE = """
SELECT json_build_object(
 'types',(SELECT count(*) FROM pg_type WHERE typname IN ('lifetime_domain','lifetime_enum','lifetime_composite','lifetime_shell')),
 'columns',(SELECT json_agg(attname ORDER BY attnum) FROM pg_attribute WHERE attrelid='lifetime_test.dependent'::regclass AND attnum>0 AND NOT attisdropped),
 'rows',(SELECT json_agg(n ORDER BY n) FROM lifetime_test.dependent),
 'view',(SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='lifetime_test' AND c.relname='dependent_view'),
 'routine',(SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='lifetime_test' AND p.proname='dependent_routine')
);
"""


def capture_timeouts(command, sql):
    cases = []
    for cleanup, setting, transaction, busy in [
        ("idle_session_timeout", "idle_session_timeout", False, False),
        ("idle_in_transaction_session_timeout", "idle_in_transaction_session_timeout", True, False),
        ("transaction_timeout", "transaction_timeout", True, False),
        ("transaction_timeout busy", "transaction_timeout", False, True),
    ]:
        sql("DROP SCHEMA IF EXISTS lifetime_test CASCADE;")
        connection = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True,
        )
        try:
            connection.stdin.write(SETUP + OBSERVE)
            connection.stdin.flush()
            before = json.loads(connection.stdout.readline())
            arm = f"SET {setting}='1000ms';"
            if transaction:
                arm += "BEGIN; UPDATE lifetime_test.dependent SET n=99;"
            running = "SELECT pg_sleep(60);" if busy else ""
            connection.stdin.write(arm + "SELECT 'ready';" + running + "\n")
            connection.stdin.flush()
            if connection.stdout.readline().strip() != "ready":
                raise RuntimeError(f"{cleanup}: failed to arm the reference timeout")
            start = time.monotonic()
            while True:
                after = json.loads(sql(OBSERVE))
                if after["types"] == 0:
                    break
                if time.monotonic() - start > 20:
                    raise RuntimeError(f"{cleanup}: temporary catalog remained after timeout: {after}")
                time.sleep(0.01)
            if not busy:
                connection.stdin.write("SELECT 1;\n")
                connection.stdin.flush()
            _, stderr = connection.communicate(timeout=20)
            fatal = re.search(r"FATAL:\s+(\w{5}):", stderr)
            if fatal is None:
                raise RuntimeError(f"{cleanup}: missing backend termination: {stderr}")
            cases.append({
                "cleanup": cleanup,
                "setup": SETUP,
                "observe": OBSERVE,
                "arm": arm,
                "busy": busy,
                "sqlstate": fatal.group(1),
                "observations": [before],
                "after_termination": [after],
                "after_close": [json.loads(sql(OBSERVE))],
            })
        finally:
            if connection.poll() is None:
                connection.kill()
            connection.communicate()
            sql("DROP SCHEMA IF EXISTS lifetime_test CASCADE;")
    return cases


def main():
    command = json.loads(os.environ["PG_TEMPORARY_LIFETIME_COMMAND"]) + [
        "-XAtq", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose"
    ]

    def sql(text):
        return subprocess.check_output(command, input=text, text=True).strip()

    version = sql("SELECT version();")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(f"Expected PostgreSQL 18, received {version}")

    cases = []
    for cleanup in ["DISCARD TEMP", "DISCARD TEMP READ ONLY", "DISCARD ALL", "close"]:
        sql("DROP SCHEMA IF EXISTS lifetime_test CASCADE;")
        text = SETUP + OBSERVE
        if cleanup != "close":
            if cleanup.startswith("DISCARD TEMP"):
                begin = "BEGIN READ ONLY;" if cleanup.endswith("READ ONLY") else "BEGIN;"
                text += begin + "DISCARD TEMP;" + OBSERVE + "ROLLBACK;" + OBSERVE
            else:
                text += cleanup + ";" + OBSERVE
        rows = [json.loads(row) for row in sql(text).splitlines()]
        after = [json.loads(row) for row in sql(OBSERVE).splitlines()]
        cases.append({
            "cleanup": cleanup,
            "setup": SETUP,
            "observe": OBSERVE,
            "observations": rows,
            "after_close": after,
        })
        sql("DROP SCHEMA lifetime_test CASCADE;")
    cases.extend(capture_timeouts(command, sql))
    fixture = Path(__file__).with_name("temporary_catalog_lifetime.expected.json")
    fixture.write_text(json.dumps({"version": version, "cases": cases}, indent=2) + "\n")
    print(json.dumps({
        "version": version,
        "cases": [{k: v for k, v in case.items() if k not in ["setup", "observe"]}
                  for case in cases],
    }, indent=2))


if __name__ == "__main__":
    main()
