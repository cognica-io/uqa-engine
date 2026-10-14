#!/usr/bin/env python3
"""Reject commits when the current, shared primary or explicit Cargo target exceeds 100 GB."""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
LIMIT_BYTES = 100_000_000_000


def target_directories() -> list[pathlib.Path]:
    output = subprocess.check_output(
        ["git", "worktree", "list", "--porcelain", "-z"], cwd=ROOT, text=True
    )
    roots = [
        pathlib.Path(field[len("worktree "):])
        for field in output.split("\0")
        if field.startswith("worktree ")
    ]
    if not roots or any(not root.is_absolute() for root in roots):
        raise ValueError("Git did not return absolute worktree roots")
    # Git lists the primary worktree first. Linked worktrees often use its cache
    # rather than their own; independent sibling caches belong to their hooks.
    targets = [ROOT / "target", roots[0] / "target"]
    if configured := os.environ.get("CARGO_TARGET_DIR"):
        target = pathlib.Path(configured)
        targets.append(target if target.is_absolute() else ROOT / target)
    return sorted({pathlib.Path(os.path.abspath(target)) for target in targets})


def allocated_bytes(target: pathlib.Path) -> int:
    try:
        target.lstat()
    except FileNotFoundError:
        return 0
    if not target.is_dir():
        raise ValueError(f"{target} is not an accessible directory")
    # Both BSD and GNU du support -k and -H. Follow a target-directory symlink,
    # count allocated blocks, and count hard-linked build artifacts only once.
    result = subprocess.run(
        ["du", "-skH", str(target)], check=True, capture_output=True, text=True
    )
    lines = result.stdout.splitlines()
    if len(lines) != 1:
        raise ValueError("du did not return one target-directory size")
    fields = lines[0].split(maxsplit=1)
    if len(fields) != 2 or not fields[0].isascii() or not fields[0].isdecimal():
        raise ValueError("du returned an invalid target-directory size")
    return int(fields[0]) * 1024


def main() -> int:
    try:
        sizes = [(target, allocated_bytes(target)) for target in target_directories()]
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Cannot verify target/ size; commit blocked: {error}", file=sys.stderr)
        return 1
    for target, used in sizes:
        if used > LIMIT_BYTES:
            print(
                f"{target} exceeds 100 GB: {used:,} allocated bytes "
                f"(limit {LIMIT_BYTES:,}). Remove unused build artifacts before committing.",
                file=sys.stderr,
            )
            return 1
    largest = max(used for _, used in sizes)
    print(f"Target cache OK (largest {largest / 1_000_000_000:.2f} GB / 100 GB across {len(sizes)} locations)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
