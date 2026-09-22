#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise overlapping SQL transactions through the actual Python artifact."""

from __future__ import annotations

import json
from pathlib import Path


ORACLE = json.loads(
    (Path(__file__).parents[1] / "parity/pg18/concurrent_writes.expected.json").read_text(
        encoding="utf-8"
    )
)
ISOLATION_LEVELS = (
    "READ COMMITTED",
    "READ UNCOMMITTED",
    "REPEATABLE READ",
    "SERIALIZABLE",
)


def _rows(engine, sql):
    return [f"{row['id']}|{row['value']}" for row in engine.sql(sql).rows]


def _statements(engine, statements, isolation):
    for statement in statements:
        engine.sql(f"BEGIN ISOLATION LEVEL {isolation}" if statement == "BEGIN" else statement)


def run_concurrent_writer_case(open_engine, path, schedule, isolation):
    sessions = []
    try:
        first = open_engine(path)
        sessions.append(first)
        _statements(first, ORACLE["setup"], isolation)
        second = first.new_session()
        sessions.append(second)
        observer = first.new_session()
        sessions.append(observer)
        _statements(first, schedule["a_before"], isolation)
        assert _rows(first, ORACLE["observe"]) == ["1|10", "2|0"]
        assert _rows(observer, ORACLE["observe"]) == ["1|0", "2|0"]

        # No first-session completion is sent until the second session commits.
        _statements(second, schedule["b"], isolation)
        assert _rows(second, ORACLE["observe"]) == schedule["before_a_end"]
        assert _rows(observer, ORACLE["observe"]) == schedule["before_a_end"]
        peer_value = 20 if isolation in ("READ COMMITTED", "READ UNCOMMITTED") else 0
        assert _rows(first, ORACLE["observe"]) == ["1|10", f"2|{peer_value}"]

        for statement in schedule["a_finish"]:
            first.sql(statement)
            if statement.startswith("ROLLBACK TO "):
                assert _rows(first, ORACLE["observe"]) == [
                    schedule["after_a_end"][0], f"2|{peer_value}"
                ]
                assert _rows(observer, ORACLE["observe"]) == schedule["before_a_end"]
        for session in sessions:
            assert _rows(session, ORACLE["observe"]) == schedule["after_a_end"]
    finally:
        for session in reversed(sessions):
            session.close()

    reopened = open_engine(path)
    try:
        assert _rows(reopened, ORACLE["observe"]) == schedule["after_a_end"]
    finally:
        reopened.close()
