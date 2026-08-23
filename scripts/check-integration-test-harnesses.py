#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Ensure consolidated Cargo test harnesses include every integration source."""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import sys
from collections import Counter


ROOT = pathlib.Path(__file__).resolve().parents[1]
PATH_MODULE = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
CARGO_TEST_TARGET = re.compile(
    r"cargo\s+test\b[^\n]*?(?:-p|--package)\s+([A-Za-z0-9_-]+)"
    r"[^\n]*?--test\s+([A-Za-z0-9_-]+)"
)
COMMAND_SUFFIXES = {".md", ".py", ".sh", ".toml", ".yaml", ".yml"}


def workspace_packages() -> list[dict[str, object]]:
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        metadata = json.loads(result.stdout)
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read Cargo workspace metadata: {error}") from error
    members = set(metadata["workspace_members"])
    return [package for package in metadata["packages"] if package["id"] in members]


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


def verify_crate(package: dict[str, object]) -> tuple[str, set[str], int, int]:
    manifest_path = pathlib.Path(str(package["manifest_path"])).resolve()
    crate_dir = manifest_path.parent
    package_name = package.get("name")
    if not isinstance(package_name, str):
        raise RuntimeError(f"package name is missing in {manifest_path}")

    tests_dir = crate_dir / "tests"
    direct_sources = {path.resolve() for path in tests_dir.glob("*.rs")}
    targets = package.get("targets")
    if not isinstance(targets, list):
        raise RuntimeError(f"Cargo metadata omits targets for {manifest_path}")
    test_targets = [
        target
        for target in targets
        if isinstance(target, dict)
        and isinstance(target.get("kind"), list)
        and "test" in target["kind"]
    ]
    if not test_targets:
        if direct_sources:
            raise RuntimeError(f"{manifest_path} has integration sources but no test target")
        return (package_name, set(), 0, 0)
    if len(test_targets) != 1:
        raise RuntimeError(
            f"{manifest_path} must expose exactly one integration test target, "
            f"found {len(test_targets)}"
        )
    target = test_targets[0]
    target_name = target.get("name")
    source_path = target.get("src_path")
    if not isinstance(target_name, str) or not isinstance(source_path, str):
        raise RuntimeError(f"Cargo metadata has an invalid test target for {manifest_path}")
    roots = [pathlib.Path(source_path).resolve()]
    target_names = {target_name}

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
    targets_by_package: dict[str, set[str]] = {}
    packages_by_manifest = {
        pathlib.Path(str(package["manifest_path"])).resolve(): package
        for package in workspace_packages()
    }
    for manifest_path in sorted(ROOT.glob("crates/*/Cargo.toml")):
        package_metadata = packages_by_manifest.get(manifest_path.resolve())
        if package_metadata is None:
            raise RuntimeError(f"{manifest_path} is not a Cargo workspace member")
        package, target_names, targets, sources = verify_crate(package_metadata)
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
    except (OSError, RuntimeError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
