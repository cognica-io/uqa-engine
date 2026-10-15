#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture private view catalog semantics using two sessions in a disposable PostgreSQL 18 database."""

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
        for isolation in ("REPEATABLE READ", "SERIALIZABLE"):
            for finish in ("COMMIT", "ROLLBACK", "ROLLBACK TO before_views; COMMIT"):
                a.execute("CREATE SCHEMA private_view_oracle")
                for session in (a, b):
                    session.execute("SET search_path = private_view_oracle")
                a.execute("CREATE TABLE t(v integer); INSERT INTO t VALUES (1); CREATE VIEW replaced AS SELECT 1 AS v; CREATE VIEW renamed_from AS SELECT 2 AS v; CREATE VIEW dropped AS SELECT 3 AS v; CREATE MATERIALIZED VIEW saved AS SELECT v FROM t; CREATE VIEW peer AS SELECT 5 AS v")
                a.execute(f"BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_views")
                a.execute("CREATE OR REPLACE VIEW replaced AS SELECT 11 AS v; ALTER VIEW renamed_from RENAME TO renamed_to; DROP VIEW dropped; UPDATE t SET v = 44; REFRESH MATERIALIZED VIEW saved; CREATE VIEW created AS SELECT 6 AS v")
                b.execute("CREATE OR REPLACE VIEW peer AS SELECT 55 AS v")
                during = a.execute("SELECT (SELECT v FROM replaced), (SELECT v FROM renamed_to), (SELECT v FROM saved), (SELECT v FROM peer), (SELECT v FROM created)")
                a.execute(finish)
                renamed = "renamed_to" if finish == "COMMIT" else "renamed_from"
                values = a.execute(f"SELECT (SELECT v FROM replaced), (SELECT v FROM {renamed}), (SELECT v FROM saved), (SELECT v FROM peer)")
                names = a.execute("SELECT relname FROM pg_class WHERE relnamespace = 'private_view_oracle'::regnamespace AND relname IN ('created','dropped','renamed_from','renamed_to') ORDER BY relname")
                peer_values = b.execute(f"SELECT (SELECT v FROM replaced), (SELECT v FROM {renamed}), (SELECT v FROM saved), (SELECT v FROM peer)")
                cases.append({"isolation": isolation, "finish": finish, "during": during, "values": values, "names": names, "peer_values": peer_values})
                a.execute("DROP SCHEMA private_view_oracle CASCADE")
        return {
            "postgresql_version_num": version[0],
            "capture_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "session_script_sha256": hashlib.sha256(Path(__file__).with_name("capture_concurrent_writes.py").read_bytes()).hexdigest(),
            "scope": "Private view replacement, rename, deletion, materialized refresh and creation beside a peer definition commit, with commit, rollback and savepoint undo.",
            "cases": cases,
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("container")
    parser.add_argument("--database", default="postgres")
    args = parser.parse_args()
    print(json.dumps(capture(args.container, args.database), indent=2))
