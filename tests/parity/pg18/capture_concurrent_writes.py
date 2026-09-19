#!/usr/bin/env python3
"""Capture independent-writer schedules using retained PostgreSQL sessions."""

# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

import argparse
import contextlib
import hashlib
import json
import queue
import subprocess
import threading
import time
import uuid
from pathlib import Path


class Session:
    def __init__(self, container, database):
        self.process = subprocess.Popen(
            ["docker", "exec", "-i", container, "psql", "-U", "postgres", "-d", database,
             "-X", "-qAt", "-v", "ON_ERROR_STOP=1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1,
        )
        self.lines = queue.Queue()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        try:
            self.execute("SET statement_timeout = '10s'")
        except BaseException:
            self.close()
            raise

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line.rstrip("\n"))
        self.lines.put(None)

    def execute(self, sql):
        marker = "uqa_done_" + uuid.uuid4().hex
        self.process.stdin.write(sql.rstrip("; \n") + ";\n\\echo " + marker + "\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + 15
        result = []
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("PostgreSQL schedule did not finish its named step")
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty as error:
                raise RuntimeError("PostgreSQL schedule did not finish its named step") from error
            if line is None:
                raise RuntimeError("PostgreSQL session ended: " + "\n".join(result))
            if line == marker:
                return result
            result.append(line)
            if len(result) > 128:
                raise RuntimeError("unexpectedly large PostgreSQL schedule output")

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            with contextlib.suppress(BrokenPipeError):
                self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.reader.join(timeout=5)
        self.process.stdout.close()


def capture(container, database):
    schema = "uqa_writer_oracle_" + uuid.uuid4().hex
    rows = "SELECT id, value FROM records ORDER BY id"
    with contextlib.ExitStack() as stack:
        a = Session(container, database)
        stack.callback(a.close)
        b = Session(container, database)
        stack.callback(b.close)
        version = a.execute("SHOW server_version_num")
        if version != ["180004"]:
            raise RuntimeError(f"expected the pinned PostgreSQL 18.4 oracle, received {version}")
        a.execute(f'CREATE SCHEMA "{schema}"')
        try:
            for session in (a, b):
                session.execute(f'SET search_path = "{schema}"')
            a.execute("CREATE TABLE records(id INT PRIMARY KEY, value INT)")
            cases = []
            for name, setup, finish in [
                ("commit", [], ["COMMIT"]),
                ("rollback", [], ["ROLLBACK"]),
                ("savepoint", ["UPDATE records SET value=5 WHERE id=1", "SAVEPOINT keep"],
                 ["ROLLBACK TO keep", "COMMIT"]),
            ]:
                a.execute("TRUNCATE records")
                a.execute("INSERT INTO records VALUES (1,0),(2,0)")
                before = ["BEGIN", *setup, "UPDATE records SET value=10 WHERE id=1"]
                independent = ["BEGIN", "UPDATE records SET value=20 WHERE id=2", "COMMIT"]
                for sql in before:
                    a.execute(sql)
                # B's commit must finish before any A termination command is sent.
                for sql in independent:
                    b.execute(sql)
                before_end = b.execute(rows)
                for sql in finish:
                    a.execute(sql)
                cases.append({"name": name, "a_before": before, "b": independent,
                              "before_a_end": before_end, "a_finish": finish,
                              "after_a_end": b.execute(rows)})
            image = subprocess.check_output(
                ["docker", "inspect", "--format", "{{.Image}}", container], text=True
            ).strip()
            return {"postgresql_version": "18.4", "image_id": image,
                    "source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    "setup": ["CREATE TABLE records(id INT PRIMARY KEY, value INT)",
                              "INSERT INTO records VALUES (1,0),(2,0)"],
                    "observe": rows, "cases": cases}
        finally:
            # These sessions and the random schema belong exclusively to this run.
            a.close()
            b.close()
            cleanup = Session(container, database)
            try:
                cleanup.execute(f'DROP SCHEMA "{schema}" CASCADE')
            finally:
                cleanup.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--database", default="postgres")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    result = capture(args.container, args.database)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(f"PostgreSQL concurrent-writer oracle: {len(result['cases'])} schedules")


if __name__ == "__main__":
    main()
