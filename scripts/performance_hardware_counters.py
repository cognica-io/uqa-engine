#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Bounded hardware observations, separate from SQL timing qualification."""

from __future__ import annotations

import csv
import math
import os
import signal
import subprocess
import threading
import time

from performance_qualification import QualificationError


EVENTS = ('cycles', 'instructions')
MAX_INTERVALS = 305  # The owning systemd measurement unit has a 300-second limit.
MAX_LINE = 2048


class HardwareCounters:
    """Observe the reserved CPUs without executing or retrying a workload."""

    def __init__(self, cpus: str):
        self.cpus = cpus
        self.intervals = []
        self.error = None
        self.process = None
        self.thread = None
        self.started = None

    def consume(self, line: str):
        if not line.strip() or line.lstrip().startswith('#'):
            return
        fields = next(csv.reader([line]))
        if len(fields) < 6 or fields[2].strip() or fields[3].strip() not in EVENTS:
            raise QualificationError(f'unexpected hardware counter output: {line[:200].strip()}')
        elapsed, count, _, event, runtime, coverage = (field.strip() for field in fields[:6])
        try:
            elapsed, count = float(elapsed), int(count)
            runtime, coverage = int(runtime), float(coverage)
        except ValueError as error:
            raise QualificationError('unavailable or malformed hardware counter') from error
        if (not math.isfinite(elapsed) or not 0 < elapsed <= MAX_INTERVALS + 1
                or count < 0 or runtime <= 0 or not math.isfinite(coverage)
                or not 0 < coverage <= 100):
            raise QualificationError('invalid hardware counter interval')
        if not self.intervals or self.intervals[-1]['elapsed_seconds'] != elapsed:
            if self.intervals and (self.intervals[-1]['elapsed_seconds'] >= elapsed
                                   or set(self.intervals[-1]['events']) != set(EVENTS)):
                raise QualificationError('unordered or incomplete hardware counter interval')
            if len(self.intervals) >= MAX_INTERVALS + 1:
                raise QualificationError('hardware counter inventory exceeded its bound')
            self.intervals.append({'elapsed_seconds': elapsed, 'events': {}})
        events = self.intervals[-1]['events']
        if event in events:
            raise QualificationError('duplicate hardware counter in one interval')
        events[event] = {'count': count, 'running_nanoseconds': runtime,
                         'running_percent': coverage}

    def observe(self):
        try:
            # A broken tool cannot create an unbounded trace or an unbounded line.
            for _ in range(2 * (MAX_INTERVALS + 1) + 16):
                line = self.process.stderr.readline(MAX_LINE + 1)
                if not line:
                    return
                if len(line) > MAX_LINE:
                    raise QualificationError('hardware counter line exceeded its bound')
                self.consume(line)
            raise QualificationError('hardware counter output exceeded its bound')
        except Exception as error:
            self.error = str(error)

    def start(self):
        self.started = time.monotonic()
        # Group the two events so their coverage is contemporaneous. Keep raw
        # counts and running coverage; multiplexed counts are not full-time work.
        command = ['/usr/bin/perf', 'stat', '-a', '-C', self.cpus,
                   '--no-big-num', '--no-scale', '-x', ',', '-I', '1000',
                   '--interval-count', str(MAX_INTERVALS), '-e', '{cycles,instructions}']
        self.process = subprocess.Popen(command, stdout=subprocess.DEVNULL,
                                        stderr=subprocess.PIPE, text=True,
                                        env={**os.environ, 'LC_ALL': 'C'})
        self.thread = threading.Thread(target=self.observe, daemon=True)
        self.thread.start()

    def finish(self) -> dict:
        if self.process is not None:
            early_exit = self.process.poll() is not None
            if not early_exit:
                try:
                    self.process.send_signal(signal.SIGINT)
                except ProcessLookupError:
                    early_exit = True
            try:
                status = self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                status = self.process.wait(timeout=5)
                self.error = self.error or 'hardware counter observer did not stop'
            if self.thread is not None and self.thread.ident is not None:
                self.thread.join(timeout=5)
                if self.thread.is_alive():
                    self.error = self.error or 'hardware counter reader did not stop'
            self.process.stderr.close()
            if early_exit or status not in (0, -signal.SIGINT, 128 + signal.SIGINT):
                self.error = self.error or f'hardware counter observer exited unexpectedly ({status})'
        if not self.intervals or set(self.intervals[-1]['events']) != set(EVENTS):
            self.error = self.error or 'missing or incomplete hardware counter evidence'
        return {'events': list(EVENTS), 'allowed_cpus': self.cpus,
                'started_monotonic_seconds': self.started, 'interval_seconds': 1,
                'intervals': self.intervals, 'sampling_error': self.error,
                'scope': 'raw grouped CPU-wide counters on reserved CPUs, including fixture, warmup and analysis; diagnostic only, not query latency or an acceptance limit'}
