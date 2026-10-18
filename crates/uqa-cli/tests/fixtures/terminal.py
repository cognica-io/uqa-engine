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
import re
import select
import signal
import threading
import time
import sys
import subprocess

WORK = Path(sys.argv[2])
BINARY = sys.argv[1]


class Terminal:
    def __init__(self, name, no_color=False, args=(), settings=None):
        self.editor_active = False
        self.control_tail = b""
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
            # Bracketed-paste mode marks each real editor invocation, including split escapes.
            controls = self.control_tail + part
            for mode in re.findall(rb"\x1b\[\?2004([hl])", controls):
                self.editor_active = mode == b"h"
            self.control_tail = controls[-7:]
            chunks.append(part)
        return b"".join(chunks)

    def read_until(self, witness, ready=None):
        result = self.control_tail if ready else b""
        deadline = time.monotonic() + 10
        while True:
            if witness in result and (ready is None or re.search(ready, result, re.S)):
                return result
            if time.monotonic() >= deadline:
                raise RuntimeError(f"terminal witness missing: {witness!r}, got {result!r}")
            result += self.read(0.1)

    def sample(self, name, data, witness=None, wait_for_ready=True):
        if data == b"\x03" and witness is None:
            witness = b"usql> "
        ready = None
        if witness is not None and wait_for_ready:
            # Redraws contain the prompt too. Observe a fresh editor invocation.
            ready = rb"\x1b\[\?2004h.*?usql> "
            if self.editor_active:
                ready = rb"\x1b\[\?2004l.*?" + ready
        os.write(self.fd, data)
        result = self.read_until(witness, ready) if witness else self.read(0.35)
        return result

    def close(self):
        self.sample("clear", b"\x03", b"usql> ")
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
    terminal.sample("catalog", b"CREATE TABLE shell_probe(shell_probe_column int);\r", b"usql> ")
    for line in (b"SELECT * FROM shell_pr\t", b"SELECT * FROM public.shell_pr\t"):
        output = terminal.sample("completion", line)
        assert b"shell_probe" in output, output
        terminal.sample("clear", b"\x03")
    output = terminal.sample("qualified column", b"SELECT t.shell_probe_c\t")
    assert b"t.shell_probe_column" in output, output
    terminal.sample("clear", b"\x03")
    terminal.sample("long query", b"SELECT pg_sleep(30);\r")
    terminal.sample("cancel query", b"\x03", b"57014")
    terminal.sample("reuse session", b"SELECT 42;\r", b"(1 row)")
    terminal.sample("explicit transaction", b"BEGIN;\r", b"usql> ")
    terminal.sample("abort transaction", b"SELECT 1/0;\r", b"22012")
    terminal.sample("recover transaction", b"ROLLBACK;\r", b"usql> ")
    terminal.sample("reuse after rollback", b"SELECT 43;\r", b"(1 row)")
    included = WORK / "interrupted.sql"
    included.write_text("SELECT 'include_ready';\nSELECT pg_sleep(30);\nSELECT 'must_not_continue';\n")
    terminal.sample("include", f"\\i '{included}'\r".encode(), b"include_ready", wait_for_ready=False)
    output = terminal.sample("interrupt include", b"\x03", b"usql> ")
    assert b"must_not_continue" not in output, output
    terminal.sample("reuse after include", b"SELECT 44;\r", b"(1 row)")
    terminal.sample("highlight off", b"\\set HIGHLIGHT off\r", b"usql> ")
    output = terminal.sample("plain input", b"SELECT")
    assert b"\x1b[1;34mSELECT" not in output, output
finally:
    terminal.close()

terminal = Terminal("delayed-shutdown")
terminal.sample("partial", b"SELECT")
# Hold a real redraw while the child is stopped; its prompt is not a new editor.
pending_redraw = [terminal.sample("redraw", b"\x0c")]
assert b"usql> " in pending_redraw[0], pending_redraw
read_terminal = terminal.read
terminal.read = lambda seconds: pending_redraw.pop(0) if pending_redraw else read_terminal(seconds)
os.kill(terminal.pid, signal.SIGSTOP)
resume = threading.Timer(1.0, lambda: os.kill(terminal.pid, signal.SIGCONT))
resume.start()
try:
    assert terminal.close() == 0
finally:
    resume.join()

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
    output = terminal.sample("buffered input", "SELECT '한글 日本語' AS buffered_input;\r".encode(), b"(1 row)")
    assert "한글 日本語".encode() in output, output
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


def pipe_witness(process, witness):
    output = b""
    deadline = time.monotonic() + 10
    while witness not in output and time.monotonic() < deadline:
        if select.select([process.stdout], [], [], 0.1)[0]:
            part = os.read(process.stdout.fileno(), 65536)
            if not part:
                break
            output += part
    assert witness in output, output
    return output


for mode in ("idle", "pipe", "file"):
    script = "SELECT 'interrupt_ready';\nSELECT pg_sleep(30);\nSELECT 'must_not_continue';\n"
    path = WORK / "batch-interrupt.sql"
    path.write_text(script)
    args = [BINARY, str(path)] if mode == "file" else [BINARY]
    process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               env={**os.environ, "UQA_HISTORY": str(WORK / "batch.history")})
    try:
        if mode == "pipe":
            process.stdin.write(script.encode())
            process.stdin.flush()
        before = pipe_witness(process, b"usql> " if mode == "idle" else b"interrupt_ready")
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired as error:
            raise AssertionError(f"SIGINT did not stop {mode} input") from error
        after, error = process.communicate(timeout=5)
        assert process.returncode == 3, (mode, process.returncode, before + after, error)
        assert b"must_not_continue" not in before + after, (mode, before + after)
    finally:
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)
print("Terminal editing, completion, cancellation, idle notifications, editor, pager and history passed")
