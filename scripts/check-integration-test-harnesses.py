#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Ensure consolidated Cargo test harnesses include every integration source."""

from __future__ import annotations

import pathlib
import re
import sys
import tomllib
from collections import Counter


ROOT = pathlib.Path(__file__).resolve().parents[1]
PATH_MODULE = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')


def test_roots(manifest: dict[str, object], crate_dir: pathlib.Path) -> list[pathlib.Path]:
    roots: list[pathlib.Path] = []
    for target in manifest.get("test", []):
        if not isinstance(target, dict) or not isinstance(target.get("name"), str):
            raise RuntimeError(f"invalid [[test]] entry in {crate_dir / 'Cargo.toml'}")
        relative = target.get("path", f"tests/{target['name']}.rs")
        if not isinstance(relative, str):
            raise RuntimeError(f"invalid test path in {crate_dir / 'Cargo.toml'}")
        roots.append((crate_dir / relative).resolve())
    return roots


def included_sources(roots: list[pathlib.Path]) -> Counter[pathlib.Path]:
    included: Counter[pathlib.Path] = Counter(roots)
    pending = list(roots)
    visited: set[pathlib.Path] = set()
    while pending:
        source_path = pending.pop()
        if source_path in visited:
            continue
        visited.add(source_path)
        try:
            source = source_path.read_text(encoding="utf-8")
        except OSError as error:
            raise RuntimeError(f"cannot read test harness source {source_path}: {error}") from error
        for relative in PATH_MODULE.findall(source):
            child = (source_path.parent / relative).resolve()
            included[child] += 1
            pending.append(child)
    return included


def verify_crate(crate_dir: pathlib.Path) -> tuple[int, int]:
    manifest_path = crate_dir / "Cargo.toml"
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    package = manifest.get("package")
    if not isinstance(package, dict) or package.get("autotests") is not False:
        return (0, 0)

    tests_dir = crate_dir / "tests"
    direct_sources = {path.resolve() for path in tests_dir.glob("*.rs")}
    roots = test_roots(manifest, crate_dir)
    if not roots:
        raise RuntimeError(f"{manifest_path} disables autotests but declares no [[test]] targets")

    included = included_sources(roots)
    missing = sorted(direct_sources - set(included))
    duplicated = sorted(path for path in direct_sources if included[path] > 1)
    if missing or duplicated:
        display = lambda paths: [path.relative_to(ROOT).as_posix() for path in paths]
        raise RuntimeError(
            f"integration test inventory differs for {crate_dir.name}; "
            f"unregistered={display(missing)}, duplicated={display(duplicated)}"
        )
    return (len(roots), len(direct_sources))


def main() -> int:
    crate_count = 0
    target_count = 0
    source_count = 0
    for manifest_path in sorted(ROOT.glob("crates/*/Cargo.toml")):
        targets, sources = verify_crate(manifest_path.parent)
        if targets:
            crate_count += 1
            target_count += targets
            source_count += sources
    print(
        "Integration harness coverage OK: "
        f"{source_count} sources in {target_count} targets across {crate_count} crates"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
