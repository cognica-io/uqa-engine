#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Select deterministic Rust regressions from their actual source and runner inputs."""

from pathlib import PurePosixPath
import sys


def requires_regression_build(paths):
    exact = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".config/nextest.toml",
             ".github/workflows/performance-platform.yml", "scripts/run-performance-checks.py",
             "scripts/tests/test_performance_checks.py"}
    for path in paths:
        if path in exact or path.startswith(("crates/", ".github/actions/setup-nextest/")):
            return True
        if path.startswith("benchmarks/regressions/") and PurePosixPath(path).suffix != ".md":
            return True
    return False


if __name__ == "__main__":
    paths = [path.decode("utf-8", "surrogateescape") for path in sys.stdin.buffer.read().split(b"\0") if path]
    print("required=" + str(requires_regression_build(paths)).lower())
