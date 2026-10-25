#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture temporary type creation, lifecycle and rollback from PostgreSQL 18."""

import json
import os
import subprocess
from pathlib import Path


def main():
    command = json.loads(os.environ["PG_TYPE_NAMESPACE_COMMAND"]) + [
        "-XAtq", "-v", "ON_ERROR_STOP=1"
    ]

    def sql(statement):
        return subprocess.check_output(command + ["-c", statement], text=True).strip()

    version = sql("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    declarations = [
        "DOMAIN object AS integer",
        "TYPE object AS ENUM ('ready')",
        "TYPE object AS (n integer)",
        "TYPE object",
    ]
    for declaration in declarations:
        for explicit in [True, False]:
            text = declaration.replace("object", "pg_temp.object") if explicit else declaration
            commands = ["BEGIN"]
            if not explicit:
                commands.append("SET LOCAL search_path=pg_temp,public")
            commands.extend([
                "CREATE " + text,
                "SELECT json_build_object('allocated',pg_my_temp_schema()<>0,'objects',"
                "(SELECT count(*) FROM pg_type WHERE typname='object' "
                "AND typnamespace=pg_my_temp_schema()))",
            ])
            if text.endswith("object"):
                commands.append("CREATE TYPE pg_temp.object AS ENUM ('completed')")
            commands.extend([
                "CREATE TEMP TABLE typed_object(v pg_temp.object,a pg_temp.object[])",
                "SELECT json_build_object('columns',(SELECT count(*) FROM pg_attribute "
                "WHERE attrelid='typed_object'::regclass AND attnum>0 "
                "AND atttypid IN ('pg_temp.object'::regtype,'pg_temp.object[]'::regtype)),"
                "'scalar',pg_typeof(NULL::pg_temp.object)::oid='pg_temp.object'::regtype::oid,"
                "'array',pg_typeof(NULL::pg_temp.object[])::oid='pg_temp.object[]'::regtype::oid)",
                "DROP TABLE typed_object",
                "ALTER TYPE pg_temp.object RENAME TO renamed",
                "DROP TYPE pg_temp.renamed",
                "SELECT json_build_object('objects',(SELECT count(*) FROM pg_type "
                "WHERE typname IN ('object','renamed') AND typnamespace=pg_my_temp_schema()))",
                "ROLLBACK",
                "SELECT json_build_object('allocated',pg_my_temp_schema()<>0)",
            ])
            statement = "; ".join(commands) + ";"
            rows = [json.loads(row) for row in sql(statement).splitlines()]
            cases.append({
                "declaration": declaration,
                "explicit": explicit,
                "sql": statement,
                "observations": rows,
            })
    destination = Path(__file__).with_name("temporary_type_namespaces.expected.json")
    destination.write_text(json.dumps({"version": version, "cases": cases}, indent=2) + "\n")
    print("Captured", len(cases), "temporary type namespace cases from", version)


if __name__ == "__main__":
    main()
