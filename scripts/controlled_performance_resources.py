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
from performance_hardware_counters import HardwareCounters


WORKQUEUE_MASK = Path('/sys/devices/virtual/workqueue/cpumask')
ADMIN_MASK = 0xff
PROC = Path('/proc')
ADDR_NO_RANDOMIZE = 0x40000


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
        'virtual_memory': {fields[0]: int(fields[1])
                           for line in (PROC / 'vmstat').read_text().splitlines()
                           if (fields := line.split())[0] in {
                               'pgfault', 'pgmajfault', 'pgscan_kswapd', 'pgscan_direct',
                               'compact_stall', 'compact_success', 'thp_collapse_alloc',
                               'thp_fault_alloc'}},
    }


class MeasurementResources:
    """Sample resource diagnostics; these are not query timings or acceptance limits."""

    def __init__(self, group: Path, output: Path, cpus: str, *, executable: Path | None = None):
        self.group, self.output, self.cpus = group, output, cpus
        self.executable = executable.resolve() if executable is not None else None
        self.layouts = {}
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.observe, daemon=True)
        self.tasks = {}
        self.samples = 0
        self.error = None
        self.before = snapshot()
        self.hardware = HardwareCounters(cpus)

    def sample_layout(self, process: str):
        if self.executable is None:
            return
        path = PROC / process
        try:
            if (path / 'exe').resolve(strict=True) != self.executable:
                return  # systemd/setarch can precede exec in the same cgroup.
            fields = (path / 'stat').read_text().rsplit(')', 1)[1].split()
            personality = int((path / 'personality').read_text().strip(), 16)
            if not personality & ADDR_NO_RANDOMIZE:
                raise QualificationError('workload address randomization is enabled')
            identity = f'{process}/{fields[19]}'
            if identity in self.layouts:
                return
            if len(self.layouts) >= 4096:
                raise QualificationError('workload layout inventory exceeded its bound')
            with (path / 'maps').open() as stream:
                mappings = stream.read(256 * 1024 + 1)
            if len(mappings) > 256 * 1024:
                raise QualificationError('workload memory-map evidence exceeded its bound')
            binary = [line for line in mappings.splitlines()
                      if line.endswith(' ' + str(self.executable))]
            if not any('r-x' in line.split()[1] for line in binary):
                raise QualificationError('workload executable mapping evidence is missing')
            self.layouts[identity] = {
                'personality': f'{personality:08x}', 'executable_mappings': binary,
                'heap_and_stack': [line for line in mappings.splitlines()
                                   if line.endswith((' [heap]', ' [stack]'))],
            }
        except (FileNotFoundError, ProcessLookupError):
            return

    def sample(self):
        try:
            processes = (self.group / 'cgroup.procs').read_text().split()
            effective = (self.group / 'cpuset.cpus.effective').read_text().strip()
        except FileNotFoundError:
            return
        if effective != self.cpus:
            raise QualificationError('workload effective CPU allocation changed')
        for process in processes:
            self.sample_layout(process)
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
                row.update(minor_faults=int(fields[7]), major_faults=int(fields[9]))
        self.samples += 1

    def observe(self):
        try:
            while not self.stopped.is_set():
                self.sample()
                self.stopped.wait(0.25)
        except Exception as error:
            self.error = str(error)

    def __enter__(self):
        try:
            self.hardware.start()
            self.thread.start()
        except BaseException:
            self.hardware.finish()
            raise
        return self

    def __exit__(self, *exception):
        self.stopped.set()
        self.thread.join()
        hardware = self.hardware.finish()
        self.error = self.error or hardware['sampling_error']
        record = {'schema_version': 1, 'allowed_cpus': self.cpus, 'before': self.before,
                  'after': snapshot(), 'sample_interval_seconds': 0.25,
                  'samples': self.samples, 'last_observed_tasks': self.tasks,
                  'fixed_layout_executable': str(self.executable) if self.executable else None,
                  'fixed_layout_processes': self.layouts,
                  'hardware_counters': hardware,
                  'sampling_error': self.error,
                  'scope': 'whole invocation including fixture, warmup and analysis; task counters end at their last observed sample, not necessarily exit'}
        if not self.tasks and not self.error:
            self.error = record['sampling_error'] = 'no workload task resource evidence was collected'
        if self.executable is not None and not self.layouts and not self.error:
            self.error = record['sampling_error'] = 'no fixed workload layout evidence was collected'
        self.output.write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')
        if self.error and exception[0] is None:
            raise QualificationError(self.error)
