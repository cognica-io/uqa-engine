#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise persistent reopen, savepoints, and sessions through Python."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import uqa


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="uqa-python-storage-") as directory:
        path = Path(directory) / "accounts.db"
        first = uqa.open(path)
        first.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT, balance INTEGER)")
        first.sql("INSERT INTO accounts (id, owner, balance) VALUES (1, 'alice', 100), (2, 'bob', 50)")
        first.close()

        engine = uqa.open(path)
        try:
            reopened_count = count(engine)
            engine.sql("BEGIN")
            engine.sql("UPDATE accounts SET balance = 0")
            inside_rollback = balance(engine, "alice")
            engine.sql("ROLLBACK")
            after_rollback = balance(engine, "alice")

            engine.sql("BEGIN")
            engine.sql("UPDATE accounts SET balance = balance - 10 WHERE owner = 'alice'")
            engine.sql("SAVEPOINT after_debit")
            engine.sql("UPDATE accounts SET balance = balance - 90 WHERE owner = 'alice'")
            before_savepoint_rollback = balance(engine, "alice")
            engine.sql("ROLLBACK TO SAVEPOINT after_debit")
            after_savepoint_rollback = balance(engine, "alice")
            engine.sql("RELEASE SAVEPOINT after_debit")
            engine.sql("COMMIT")

            session = engine.new_session()
            try:
                session_balance = balance(session, "alice")
            finally:
                session.close()
            results = {
                "reopened_count": reopened_count,
                "inside_rollback": inside_rollback,
                "after_rollback": after_rollback,
                "before_savepoint_rollback": before_savepoint_rollback,
                "after_savepoint_rollback": after_savepoint_rollback,
                "session_balance": session_balance,
            }
            assert results == {
                "reopened_count": 2,
                "inside_rollback": 0,
                "after_rollback": 100,
                "before_savepoint_rollback": 0,
                "after_savepoint_rollback": 90,
                "session_balance": 90,
            }
            print(json.dumps(results, sort_keys=True))
        finally:
            engine.close()


def count(engine: object) -> int:
    return engine.sql("SELECT COUNT(*) AS n FROM accounts").rows[0]["n"]


def balance(engine: object, owner: str) -> int:
    return engine.sql("SELECT balance FROM accounts WHERE owner = $1", [owner]).rows[0]["balance"]


if __name__ == "__main__":
    main()
