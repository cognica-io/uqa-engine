#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Select deterministic Rust regressions from their actual source and runner inputs."""

import argparse
from pathlib import Path, PurePosixPath
import re
import subprocess
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


def requires_regex_build(paths):
    exact = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "scripts/emscripten-flags.py",
             ".github/workflows/regular-expression-checks.yml"}
    return any(path in exact or path.startswith("crates/uqa-pg-regex/") for path in paths)


def requires_dependency_check(paths):
    exact = {"Cargo.toml", "Cargo.lock", "deny.toml", ".github/workflows/dependency-checks.yml"}
    return any(path in exact or (path.startswith("crates/") and path.endswith("/Cargo.toml"))
               for path in paths)


def changed_paths(output):
    return [path.decode("utf-8", "surrogateescape") for path in output.split(b"\0") if path]


def requires_revision_build(base, before=None, *, repository=".", current="HEAD",
                            select=requires_regression_build):
    """Keep earlier product evidence only when both head and base inputs are unchanged."""
    if not re.fullmatch(r"[0-9a-f]{40}", base or "") or not base.strip("0"):
        return True

    def git(*args, **kwargs):
        return subprocess.run(["git", "-C", str(repository), *args],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              check=True, **kwargs).stdout

    try:
        if before is None:
            return select(changed_paths(git(
                "diff", "--name-only", "-z", "--no-renames", base, current, "--")))
        if not re.fullmatch(r"[0-9a-f]{40}", before) or not before.strip("0"):
            return True
        if select(changed_paths(git(
                "diff", "--name-only", "-z", "--no-renames", before, current, "--"))):
            return True

        # A previous merge run may have included newer base changes that were
        # subsequently reverted. Inspect the base history, not only its final tree.
        common = git("merge-base", before, base).decode().strip()
        commits = git("rev-list", "--full-history", base, "--not", common)
        if not commits:
            return False
        shallow_file = Path(git("rev-parse", "--path-format=absolute", "--git-path", "shallow").decode().strip())
        if shallow_file.exists() and set(commits.splitlines()) & set(shallow_file.read_bytes().splitlines()):
            return True
        return select(changed_paths(git(
            "diff-tree", "--stdin", "--root", "-m", "--no-commit-id", "--name-only",
            "-r", "-z", "--no-renames", input=commits)))
    except subprocess.CalledProcessError:
        # Missing/shallow history cannot establish equivalence to the earlier run.
        return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--before")
    parser.add_argument("--kind", choices=["regression", "regex", "dependencies"], default="regression")
    args = parser.parse_args()
    select = {"regression": requires_regression_build, "regex": requires_regex_build,
              "dependencies": requires_dependency_check}[args.kind]
    required = (requires_revision_build(args.base, args.before, select=select) if args.base is not None else
                select(changed_paths(sys.stdin.buffer.read())))
    print("required=" + str(required).lower())
