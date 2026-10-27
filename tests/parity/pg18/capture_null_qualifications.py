#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture NULL qualification effects separately from scalar truth observations."""

import json
import os
import subprocess
from pathlib import Path


def main():
    command = json.loads(os.environ["PG_QUALIFICATION_COMMAND"]) + [
        "-XAtq", "-v", "ON_ERROR_STOP=1"
    ]

    def sql(statement):
        return subprocess.check_output(command + ["-c", statement], text=True).strip()

    version = sql("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    setup = (
        "CREATE TEMP SEQUENCE qual_ticks; "
        "CREATE TEMP TABLE qual_input(n integer PRIMARY KEY); "
        "INSERT INTO qual_input VALUES(1),(2),(3); "
        "CREATE TEMP TABLE qual_output(n integer,b boolean)"
    )
    nullable = "nextval('qual_ticks') > 0 AND NULL::boolean"
    operations = [
        ("where", "", f"INSERT INTO qual_output(n) SELECT n FROM qual_input WHERE {nullable}"),
        ("between", "", "INSERT INTO qual_output(n) SELECT n FROM qual_input WHERE nextval('qual_ticks') BETWEEN SYMMETRIC NULL::bigint AND 100"),
        ("update", "", f"UPDATE qual_input SET n=n+10 WHERE {nullable}"),
        ("delete", "", f"DELETE FROM qual_input WHERE {nullable}"),
        ("having", "", f"INSERT INTO qual_output(n) SELECT count(*)::integer FROM qual_input HAVING {nullable}"),
        ("join", "", f"INSERT INTO qual_output(n) SELECT i.n FROM qual_input i JOIN (VALUES(1),(2)) s(n) ON ({nullable})"),
        ("or", "", "INSERT INTO qual_output(n) SELECT n FROM qual_input WHERE nextval('qual_ticks') > 0 OR NULL::boolean"),
        ("projection", "", f"INSERT INTO qual_output SELECT n,({nullable}) FROM qual_input"),
        ("boolean-test", "", f"INSERT INTO qual_output(n) SELECT n FROM qual_input WHERE ({nullable}) IS NOT FALSE"),
        ("not", "", f"INSERT INTO qual_output(n) SELECT n FROM qual_input WHERE NOT ({nullable})"),
        ("conflict", "", f"INSERT INTO qual_input VALUES(1) ON CONFLICT(n) DO UPDATE SET n=10 WHERE {nullable}"),
        ("merge-join", "", f"MERGE INTO qual_input i USING (VALUES(1)) s(n) ON ({nullable}) WHEN MATCHED THEN UPDATE SET n=10"),
        ("merge-when", "", f"MERGE INTO qual_input i USING (VALUES(1)) s(n) ON i.n=s.n WHEN MATCHED AND ({nullable}) THEN UPDATE SET n=10"),
        ("rule", "CREATE RULE route AS ON INSERT TO qual_input WHERE (nextval('qual_ticks') BETWEEN SYMMETRIC NULL::bigint AND 100) DO ALSO INSERT INTO qual_output(n) VALUES(NEW.n)", "INSERT INTO qual_input VALUES(4)"),
    ]
    observe = (
        "SELECT json_build_object('calls',(SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM qual_ticks),"
        "'input',(SELECT json_agg(n ORDER BY n) FROM qual_input),"
        "'output',(SELECT json_agg(json_build_array(n,b) ORDER BY n) FROM qual_output))"
    )
    cases = []
    for name, extra, operation in operations:
        statements = [setup, extra, operation, observe]
        observed = sql("BEGIN; " + "; ".join(statements) + "; ROLLBACK;")
        cases.append({
            "name": name,
            "setup": setup + "; " + extra,
            "operation": operation,
            "observe": observe,
            "expected": json.loads(observed),
        })
    destination = Path(__file__).with_name("null_qualifications.expected.json")
    destination.write_text(json.dumps({"version": version, "cases": cases}, indent=2) + "\n")
    print("Captured", len(cases), "NULL qualification cases from", version)


if __name__ == "__main__":
    main()
