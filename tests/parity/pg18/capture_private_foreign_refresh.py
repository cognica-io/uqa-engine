#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture private foreign-definition lifecycle using two PostgreSQL 18 sessions."""

import argparse
import contextlib
import hashlib
import json
from pathlib import Path

from capture_concurrent_writes import Session


def capture(container, database):
    cases = []
    with contextlib.ExitStack() as stack:
        a = Session(container, database)
        stack.callback(a.close)
        b = Session(container, database)
        stack.callback(b.close)
        version = a.execute("SHOW server_version_num")
        if version != ["180004"]:
            raise RuntimeError(f"expected PostgreSQL 18.4, received {version}")
        for session in (a, b):
            session.execute("SET client_min_messages = warning")
        # No scans are needed: an ordinary handlerless wrapper gives PostgreSQL the same catalog-only declarations as UQA's built-in memory wrapper.
        a.execute("CREATE FOREIGN DATA WRAPPER memory_fdw; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw")
        for isolation in ("REPEATABLE READ", "SERIALIZABLE"):
            for finish in ("COMMIT", "ROLLBACK", "ROLLBACK TO before_foreign; COMMIT"):
                a.execute("CREATE SCHEMA private_foreign_oracle")
                for session in (a, b):
                    session.execute("SET search_path = private_foreign_oracle")
                a.execute("CREATE TABLE t(v int); INSERT INTO t VALUES (1)")
                for name in ("replaced", "renamed_from", "dropped", "recreated", "peer"):
                    columns = "v int, extra text" if name in ("replaced", "peer") else "v int"
                    a.execute(f"CREATE FOREIGN TABLE {name}({columns}) SERVER remote")
                a.execute(f"BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_foreign")
                a.execute("ALTER FOREIGN TABLE replaced DROP COLUMN extra; GRANT SELECT(v) ON replaced TO PUBLIC; ALTER FOREIGN TABLE renamed_from RENAME TO renamed_to; DROP FOREIGN TABLE dropped; DROP FOREIGN TABLE recreated; CREATE FOREIGN TABLE recreated(v text) SERVER remote; CREATE FOREIGN TABLE created(v int) SERVER remote")
                b.execute("ALTER FOREIGN TABLE peer DROP COLUMN extra")
                a.execute(finish)
                query = "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod) FROM pg_class c JOIN pg_attribute a ON a.attrelid=c.oid WHERE c.relnamespace='private_foreign_oracle'::regnamespace AND c.relkind='f' AND a.attnum>0 AND NOT a.attisdropped ORDER BY c.relname, a.attnum"
                columns = a.execute(query)
                peer_columns = b.execute(query)
                cases.append({"isolation": isolation, "finish": finish, "columns": columns, "peer_columns": peer_columns})
                a.execute("DROP SCHEMA private_foreign_oracle CASCADE")
        return {
            "postgresql_version_num": version[0],
            "capture_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "session_script_sha256": hashlib.sha256(Path(__file__).with_name("capture_concurrent_writes.py").read_bytes()).hexdigest(),
            "scope": "Private foreign creation, alteration, rename, deletion and recreation beside a peer definition commit, with commit, rollback and savepoint undo; a handlerless wrapper is used only for catalog DDL.",
            "cases": cases,
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("container")
    parser.add_argument("--database", required=True)
    args = parser.parse_args()
    print(json.dumps(capture(args.container, args.database), indent=2))
