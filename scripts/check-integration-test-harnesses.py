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
CARGO_TEST_TARGET = re.compile(
    r"cargo\s+test\b[^\n]*?(?:-p|--package)\s+([A-Za-z0-9_-]+)"
    r"[^\n]*?--test\s+([A-Za-z0-9_-]+)"
)
COMMAND_SUFFIXES = {".md", ".py", ".sh", ".toml", ".yaml", ".yml"}


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


def verify_crate(crate_dir: pathlib.Path) -> tuple[str | None, set[str], int, int]:
    manifest_path = crate_dir / "Cargo.toml"
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    package = manifest.get("package")
    if not isinstance(package, dict):
        return (None, set(), 0, 0)
    package_name = package.get("name")
    if not isinstance(package_name, str):
        raise RuntimeError(f"package name is missing in {manifest_path}")

    tests_dir = crate_dir / "tests"
    direct_sources = {path.resolve() for path in tests_dir.glob("*.rs")}
    explicit_roots = test_roots(manifest, crate_dir)
    if package.get("autotests") is False:
        if not explicit_roots:
            raise RuntimeError(
                f"{manifest_path} disables autotests but declares no [[test]] targets"
            )
        if len(explicit_roots) != 1:
            raise RuntimeError(
                f"{manifest_path} must declare exactly one [[test]] target, "
                f"found {len(explicit_roots)}"
            )
        roots = explicit_roots
        target_names = {target["name"] for target in manifest["test"]}
    else:
        if explicit_roots:
            raise RuntimeError(
                f"{manifest_path} declares [[test]] without disabling automatic test discovery"
            )
        if not direct_sources:
            return (package_name, set(), 0, 0)
        if len(direct_sources) != 1:
            raise RuntimeError(
                f"{manifest_path} must expose exactly one automatically discovered integration "
                f"test target, found {len(direct_sources)}"
            )
        roots = sorted(direct_sources)
        target_names = {root.stem for root in roots}

    included = included_sources(roots)
    missing = sorted(direct_sources - set(included))
    duplicated = sorted(path for path in direct_sources if included[path] > 1)
    if missing or duplicated:
        display = lambda paths: [path.relative_to(ROOT).as_posix() for path in paths]
        raise RuntimeError(
            f"integration test inventory differs for {crate_dir.name}; "
            f"unregistered={display(missing)}, duplicated={display(duplicated)}"
        )
    return (package_name, target_names, len(roots), len(direct_sources))


def command_files() -> list[pathlib.Path]:
    files = [path for path in ROOT.glob("*.md") if path.is_file()]
    for directory in (ROOT / ".github", ROOT / "crates", ROOT / "docs", ROOT / "scripts"):
        files.extend(
            path
            for path in directory.rglob("*")
            if path.is_file() and path.suffix in COMMAND_SUFFIXES
        )
    return sorted(set(files))


def verify_command_targets(targets_by_package: dict[str, set[str]]) -> None:
    stale = []
    for path in command_files():
        source = path.read_text(encoding="utf-8")
        for match in CARGO_TEST_TARGET.finditer(source):
            package, target = match.groups()
            allowed = targets_by_package.get(package, set())
            if target not in allowed:
                line = source.count("\n", 0, match.start()) + 1
                stale.append(
                    f"{path.relative_to(ROOT).as_posix()}:{line}: "
                    f"{package} has no integration test target {target!r}"
                )
    if stale:
        raise RuntimeError("stale Cargo test target references:\n" + "\n".join(stale))


def main() -> int:
    crate_count = 0
    target_count = 0
    source_count = 0
    targets_by_package = {}
    for manifest_path in sorted(ROOT.glob("crates/*/Cargo.toml")):
        package, target_names, targets, sources = verify_crate(manifest_path.parent)
        if package is not None:
            targets_by_package[package] = target_names
        if targets:
            crate_count += 1
            target_count += targets
            source_count += sources
    verify_command_targets(targets_by_package)
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
