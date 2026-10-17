#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the real Unix terminal editor, signals and idle notifications."""

import errno
import os
from pathlib import Path
import pty
import select
import signal
import time
import sys
import subprocess

WORK = Path(sys.argv[2])
BINARY = sys.argv[1]


class Terminal:
    def __init__(self, name, no_color=False, args=(), settings=None):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(WORK)
            env = {**os.environ, "TERM": "xterm-256color", "UQA_HISTORY": str(WORK / (name + ".history"))}
            env.update(settings or {})
            env.pop("NO_COLOR", None)
            if no_color:
                env["NO_COLOR"] = "1"
            os.execve(BINARY, [BINARY, *args], env)
        self.started = self.read_until(b"usql> ")


    def read(self, seconds):
        deadline = time.monotonic() + seconds
        chunks = []
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.fd], [], [], max(0, deadline - time.monotonic()))
            if not ready:
                break
            try:
                part = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not part:
                break
            if b"\x1b[6n" in part:
                os.write(self.fd, b"\x1b[1;1R")
            chunks.append(part)
        return b"".join(chunks)

    def read_until(self, witness):
        result = b""
        deadline = time.monotonic() + 10
        while witness not in result and time.monotonic() < deadline:
            result += self.read(0.1)
        if witness not in result:
            raise RuntimeError(f"terminal witness missing: {witness!r}, got {result!r}")
        return result

    def sample(self, name, data, witness=None):
        os.write(self.fd, data)
        result = self.read_until(witness) if witness else self.read(0.35)
        return result

    def close(self):
        self.sample("clear", b"\x03")
        self.sample("quit", b"\\q\r")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                os.close(self.fd)
                return os.waitstatus_to_exitcode(status)
            time.sleep(0.05)
        os.kill(self.pid, signal.SIGTERM)
        os.waitpid(self.pid, 0)
        os.close(self.fd)
        raise RuntimeError("terminal did not quit")


terminal = Terminal("editing")
try:
    terminal.sample("quoted bracket", b"SELECT '(' AS value;\r", b"(1 row)")
    for line in (b"/* SELECT 1 */", b"$$ SELECT 1 $$", b'"SELECT"'):
        output = terminal.sample("literal", line)
        assert b"\x1b[1;34mSELECT" not in output, output
        terminal.sample("clear", b"\x03")
    terminal.sample("multiline", b"SELECT 'first\r")
    output = terminal.sample("string continuation", b"SELECT")
    assert b"\x1b[1;34mSELECT" not in output, output
    terminal.sample("clear", b"\x03")
    terminal.sample("catalog", b"CREATE TABLE shell_probe(id int);\r", b"usql> ")
    for line in (b"SELECT * FROM shell_pr\t", b"SELECT * FROM public.shell_pr\t"):
        output = terminal.sample("completion", line)
        assert b"shell_probe" in output, output
        terminal.sample("clear", b"\x03")
    terminal.sample("long query", b"SELECT pg_sleep(30);\r")
    terminal.sample("cancel query", b"\x03", b"57014")
    terminal.sample("reuse session", b"SELECT 42;\r", b"(1 row)")
    terminal.sample("explicit transaction", b"BEGIN;\r", b"usql> ")
    terminal.sample("abort transaction", b"SELECT 1/0;\r", b"22012")
    terminal.sample("recover transaction", b"ROLLBACK;\r", b"usql> ")
    terminal.sample("reuse after rollback", b"SELECT 43;\r", b"(1 row)")
    terminal.sample("highlight off", b"\\set HIGHLIGHT off\r", b"usql> ")
    output = terminal.sample("plain input", b"SELECT")
    assert b"\x1b[1;34mSELECT" not in output, output
finally:
    terminal.close()

terminal = Terminal("no-color", no_color=True)
try:
    output = terminal.sample("plain", b"SELECT")
    assert b"\x1b[1;34mSELECT" not in output, output
finally:
    terminal.close()

path = WORK / "notifications.db"
terminal = Terminal("notifications", args=("--db", str(path)))
try:
    terminal.sample("listen", b"LISTEN shell_events;\r", b"usql> ")
    env = dict(os.environ, UQA_HISTORY=str(WORK / "sender.history"))
    subprocess.run([BINARY,"--db",str(path),"-c","NOTIFY shell_events, 'idle delivery'"], env=env, check=True, capture_output=True, timeout=10)
    output=terminal.read_until(b"idle delivery")
    assert b"Asynchronous notification" in output, output
    terminal.sample("unlisten", b"UNLISTEN *;\r", b"usql> ")
finally:
    terminal.close()
editor = WORK / "fake editor.py"
pager = WORK / "fake pager.py"
paged = WORK / "paged.txt"
editor.write_text("import pathlib,sys\npathlib.Path(sys.argv[-1]).write_text('SELECT 123 AS from_editor;\\n')\n")
pager.write_text("import pathlib,sys\npathlib.Path(sys.argv[1]).write_text(sys.stdin.read())\n")
terminal = Terminal("processes", settings={"PSQL_EDITOR": f"python3 '{editor}'", "PSQL_PAGER": f"python3 '{pager}' '{paged}'"})
try:
    terminal.sample("disable pager", b"\\pset pager off\r", b"usql> ")
    terminal.sample("edit query", b"\\e\r", b"from_editor")
    terminal.sample("pager always", b"\\pset pager always\r", b"usql> ")
    terminal.sample("page result", b"SELECT 55 AS paged_value;\r", b"usql> ")
    assert "paged_value" in paged.read_text() and "55" in paged.read_text()
    terminal.sample("clear history", b"\\history clear\r", b"usql> ")
finally:
    terminal.close()
assert "from_editor" not in (WORK / "processes.history").read_text()
print("Terminal editing, completion, cancellation, idle notifications, editor, pager and history passed")
