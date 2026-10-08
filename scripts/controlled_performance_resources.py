#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Bounded, controller-owned resource evidence for each workload invocation."""

from __future__ import annotations

import json
from pathlib import Path
import threading
import time

from performance_qualification import QualificationError


WORKQUEUE_MASK = Path('/sys/devices/virtual/workqueue/cpumask')
ADMIN_MASK = 0xff
PROC = Path('/proc')


def workqueue_mask() -> int:
    return int(WORKQUEUE_MASK.read_text().strip().replace(',', ''), 16)


def restrict_workqueues():
    WORKQUEUE_MASK.write_text(f'{ADMIN_MASK:x}')
    verify_workqueues()


def verify_workqueues():
    if workqueue_mask() != ADMIN_MASK:
        raise QualificationError('unbound kernel work can enter the measurement CPU set')


def snapshot() -> dict:
    return {
        'monotonic_seconds': time.monotonic(),
        'cpu_ticks': {fields[0]: list(map(int, fields[1:]))
                      for line in (PROC / 'stat').read_text().splitlines()
                      if (fields := line.split())[0] in {f'cpu{cpu}' for cpu in range(8, 16)}},
        'pressure': {name: (PROC / 'pressure' / name).read_text()
                     for name in ('cpu', 'memory', 'io')},
    }


class MeasurementResources:
    """Sample resource diagnostics; these are not query timings or acceptance limits."""

    def __init__(self, group: Path, output: Path, cpus: str):
        self.group, self.output, self.cpus = group, output, cpus
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.observe, daemon=True)
        self.tasks = {}
        self.samples = 0
        self.error = None
        self.before = snapshot()

    def sample(self):
        try:
            processes = (self.group / 'cgroup.procs').read_text().split()
            effective = (self.group / 'cpuset.cpus.effective').read_text().strip()
        except FileNotFoundError:
            return
        if effective != self.cpus:
            raise QualificationError('workload effective CPU allocation changed')
        for process in processes:
            for task in (PROC / process / 'task').glob('[0-9]*'):
                try:
                    fields = (task / 'stat').read_text().rsplit(')', 1)[1].split()
                    running, waiting, switches = map(int, (task / 'schedstat').read_text().split())
                except (FileNotFoundError, ProcessLookupError):
                    continue
                identity = f'{process}/{task.name}/{fields[19]}'
                if identity not in self.tasks:
                    if len(self.tasks) >= 4096:
                        raise QualificationError('workload diagnostic task inventory exceeded its bound')
                    self.tasks[identity] = {'sampled_cpus': {}, 'CPU_nanoseconds': 0,
                                            'run_queue_nanoseconds': 0, 'timeslices': 0}
                row = self.tasks[identity]
                cpu = fields[36]
                row['sampled_cpus'][cpu] = row['sampled_cpus'].get(cpu, 0) + 1
                row.update(CPU_nanoseconds=running, run_queue_nanoseconds=waiting, timeslices=switches)
        self.samples += 1

    def observe(self):
        try:
            while not self.stopped.is_set():
                self.sample()
                self.stopped.wait(0.25)
        except Exception as error:
            self.error = str(error)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exception):
        self.stopped.set()
        self.thread.join()
        record = {'schema_version': 1, 'allowed_cpus': self.cpus, 'before': self.before,
                  'after': snapshot(), 'sample_interval_seconds': 0.25,
                  'samples': self.samples, 'last_observed_tasks': self.tasks,
                  'sampling_error': self.error,
                  'scope': 'whole invocation including fixture, warmup and analysis; task counters end at their last observed sample, not necessarily exit'}
        if not self.tasks and not self.error:
            self.error = record['sampling_error'] = 'no workload task resource evidence was collected'
        self.output.write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')
        if self.error and exception[0] is None:
            raise QualificationError(self.error)
