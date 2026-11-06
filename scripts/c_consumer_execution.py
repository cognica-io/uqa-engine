#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run independently owned native-consumer cases with bounded concurrency."""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
import subprocess


def run_cases(cases: list[tuple[str, list[str], dict[str, str]]], jobs: int) -> int:
    if jobs < 1:
        raise ValueError("jobs must be positive")
    if not cases:
        raise ValueError("No native consumers selected")
    failures = []
    executor = ThreadPoolExecutor(max_workers=jobs)
    try:
        pending = {
            executor.submit(subprocess.run, command, env=environment,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, errors="replace"): label
            for label, command, environment in cases
        }
        for future in as_completed(pending):
            label = pending[future]
            try:
                result = future.result()
            except OSError as error:
                failures.append(label)
                print(f"Failed {label}: {error}", flush=True)
                continue
            status = f"Passed {label}" if result.returncode == 0 else f"Failed {label} (exit {result.returncode})"
            print(status, flush=True)
            if result.stdout:
                print(result.stdout, end="", flush=True)
            if result.returncode:
                failures.append(label)
    finally:
        executor.shutdown(wait=True, cancel_futures=True)
    if failures:
        raise RuntimeError("Native consumers failed: " + ", ".join(sorted(failures)))
    return len(cases)
