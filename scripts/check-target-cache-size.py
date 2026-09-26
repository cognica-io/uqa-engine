#!/usr/bin/env python3
"""Reject commits when the worktree's target directory exceeds 100 GB on disk."""

from __future__ import annotations

import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
LIMIT_BYTES = 100_000_000_000


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
        used = allocated_bytes(ROOT / "target")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Cannot verify target/ size; commit blocked: {error}", file=sys.stderr)
        return 1
    if used > LIMIT_BYTES:
        print(
            f"target/ exceeds 100 GB: {used:,} allocated bytes "
            f"(limit {LIMIT_BYTES:,}). Remove unused build artifacts before committing.",
            file=sys.stderr,
        )
        return 1
    print(f"Target cache OK ({used / 1_000_000_000:.2f} GB / 100 GB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
