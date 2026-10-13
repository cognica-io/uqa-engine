#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check published text while distinguishing Rust syntax from literal data."""

from __future__ import annotations

import bisect
import fnmatch
import os
import pathlib
import re
import subprocess
import sys


MACHINE_PATH = re.compile(r"/Users/[A-Za-z0-9_.-]+/|/home/[A-Za-z0-9_.-]+/|[A-Za-z]:\\Users\\|file:///(?:Users|home)/")
CREDENTIAL = re.compile(r"github_pat_[A-Za-z0-9_]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9_-]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")
INITIALISM = re.compile(r"(?<![A-Za-z0-9_])(?:[C]pu|[M]lx|[U]qa)[A-Za-z0-9_]*(?![A-Za-z0-9_])")
PRIVATE_HOST = re.compile(r"(?<![A-Za-z0-9_.-])(?:[A-Za-z0-9][A-Za-z0-9_-]*\.)+(?:internal|local)(?![A-Za-z0-9_.(-])|https?://(?:10\.|192\.168\.|172\.(?:1[6-9]|2[0-9]|3[01])\.)")
RUST_TOKEN = re.compile(r'''//|/\*|\b(?:br|cr|r)(?P<hashes>\#{0,255})"|(?P<character>'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[\s\S])|[^'\\\n])')|"''')
BLOCK_TOKEN = re.compile(r"/\*|\*/")
STRING_TOKEN = re.compile(r'\\[\s\S]|"')
SCANNERS = {"scripts/check-public-repository-hygiene.sh", "scripts/check-public-repository-hygiene.py"}
GENERATED = ("benchmarks/nori/evidence/**", "benchmarks/nori/*-evidence/**", "benchmarks/nori/*followup-evidence.json")


def rust_text_ranges(source: str) -> list[tuple[int, int]]:
    """Locate comments and string contents without interpreting source identifiers."""
    spans = []
    cursor = 0
    while token := RUST_TOKEN.search(source, cursor):
        start, cursor = token.span()
        spelling = token.group()
        if token.group("character") is not None:
            continue
        if spelling == "//":
            end = source.find("\n", cursor)
            cursor = len(source) if end < 0 else end
        elif spelling == "/*":
            depth = 1
            while depth and (boundary := BLOCK_TOKEN.search(source, cursor)):
                depth += 1 if boundary.group() == "/*" else -1
                cursor = boundary.end()
            if depth:
                cursor = len(source)
        elif token.group("hashes") is not None:
            delimiter = '"' + token.group("hashes")
            end = source.find(delimiter, cursor)
            cursor = len(source) if end < 0 else end + len(delimiter)
        else:
            while boundary := STRING_TOKEN.search(source, cursor):
                cursor = boundary.end()
                if boundary.group() == '"':
                    break
            else:
                cursor = len(source)
        spans.append((start, cursor))
    return spans


def violating_lines(name: str, source: str) -> list[int]:
    positions = {match.start() for pattern in (MACHINE_PATH, CREDENTIAL, INITIALISM)
                 for match in pattern.finditer(source)}
    hosts = list(PRIVATE_HOST.finditer(source))
    spans = rust_text_ranges(source) if hosts and name.endswith(".rs") else None
    starts = [start for start, _ in spans] if spans is not None else []
    for match in hosts:
        if spans is not None:
            index = bisect.bisect_right(starts, match.start()) - 1
            if index < 0 or match.start() >= spans[index][1]:
                continue
        positions.add(match.start())
    return sorted({source.count("\n", 0, position) + 1 for position in positions})


def check_repository(root: pathlib.Path) -> list[str]:
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root
    ).split(b"\0")
    violations = []
    for raw in sorted(set(paths) - {b""}):
        name = os.fsdecode(raw)
        path = root / name
        if name in SCANNERS or not (path.is_symlink() or path.is_file()):
            continue
        if any(fnmatch.fnmatchcase(name, pattern) for pattern in GENERATED):
            violations.append(f"{name}: generated benchmark reports belong in target/benchmark-runs or CI artifacts")
        if path.is_symlink():
            source = os.readlink(path)
            source_name = "symlink"
        else:
            with path.open("rb") as stream:
                prefix = stream.read(8192)
                if b"\0" in prefix:
                    continue
                content = prefix + stream.read()
            if b"\0" in content:
                continue
            source = content.decode("utf-8", errors="replace")
            source_name = name
        lines = source.split("\n")
        violations.extend(f"{name}:{line}:{lines[line - 1]}"
                          for line in violating_lines(source_name, source))
    return violations


def main() -> int:
    violations = check_repository(pathlib.Path.cwd())
    if violations:
        print("Public repository hygiene violations found:", file=sys.stderr)
        print("\n".join(violations), file=sys.stderr)
        return 1
    print("Public repository hygiene OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
