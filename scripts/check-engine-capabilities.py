#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Enforce ownership allowlists for migrated Engine capability boundaries."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "scripts" / "engine-capability-policy.json"
ENGINE_REFERENCE = re.compile(r"\bEngine\b")
CAPABILITY_ESCAPE_HATCHES = (
    (
        re.compile(r"\b(?:std::ops::)?Deref(?:Mut)?\b"),
        "capability module must not implement or import Deref",
    ),
    (
        re.compile(
            r"\bengine\s*:\s*&(?:\s*'[A-Za-z_][A-Za-z0-9_]*\s+)?"
            r"(?:[A-Za-z_][A-Za-z0-9_]*::)*Engine\b"
        ),
        "capability module must not retain an engine reference",
    ),
    (
        re.compile(r"\b(?:EngineServices|EngineContext|EngineCapabilitySet)\b"),
        "capability module must not define a catch-all engine service type",
    ),
    (
        re.compile(r"(?m)\bfn\s+(?:engine|as_engine|into_engine)\s*\("),
        "capability module must not expose an engine recovery method",
    ),
)


class PolicyError(RuntimeError):
    """The capability policy or current source tree is invalid."""


def normalized_path(value: object, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise PolicyError(f"{field} must be a non-empty relative path")
    path = pathlib.PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or path.as_posix() != value:
        raise PolicyError(f"{field} must be a normalized relative path: {value!r}")
    return value


def sorted_unique_paths(value: object, field: str) -> list[str]:
    if not isinstance(value, list):
        raise PolicyError(f"{field} must be a list")
    paths = [normalized_path(path, f"{field} entry") for path in value]
    if paths != sorted(paths):
        raise PolicyError(f"{field} must be sorted")
    if len(paths) != len(set(paths)):
        raise PolicyError(f"{field} contains duplicates")
    return paths


def load_policy(path: pathlib.Path) -> dict[str, Any]:
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyError(f"cannot read capability policy {path}: {error}") from error
    if not isinstance(policy, dict) or set(policy) != {
        "schema_version",
        "capability_module",
        "scopes",
    }:
        raise PolicyError("capability policy has unexpected top-level fields")
    if policy["schema_version"] != 1:
        raise PolicyError("capability policy must use schema_version 1")
    policy["capability_module"] = normalized_path(
        policy["capability_module"], "capability_module"
    )
    scopes = policy["scopes"]
    if not isinstance(scopes, list) or not scopes:
        raise PolicyError("scopes must be a non-empty list")
    names: set[str] = set()
    covered: set[str] = set()
    for index, scope in enumerate(scopes):
        field = f"scopes[{index}]"
        if not isinstance(scope, dict) or set(scope) != {
            "name",
            "files",
            "engine_allowlist",
        }:
            raise PolicyError(f"{field} has unexpected fields")
        name = scope["name"]
        if not isinstance(name, str) or not name.strip():
            raise PolicyError(f"{field}.name must be non-empty")
        if name in names:
            raise PolicyError(f"duplicate scope name: {name}")
        names.add(name)
        files = sorted_unique_paths(scope["files"], f"{field}.files")
        if not files:
            raise PolicyError(f"{field}.files must not be empty")
        overlap = covered.intersection(files)
        if overlap:
            raise PolicyError(f"files occur in multiple scopes: {sorted(overlap)}")
        covered.update(files)
        allowlist = sorted_unique_paths(
            scope["engine_allowlist"], f"{field}.engine_allowlist"
        )
        unexpected = set(allowlist).difference(files)
        if unexpected:
            raise PolicyError(
                f"{field}.engine_allowlist contains files outside the scope: {sorted(unexpected)}"
            )
        scope["files"] = files
        scope["engine_allowlist"] = allowlist
    if policy["capability_module"] not in covered:
        raise PolicyError("capability_module must belong to a declared scope")
    return policy


def verify(root: pathlib.Path, policy_path: pathlib.Path) -> dict[str, int]:
    policy = load_policy(policy_path)
    errors: list[str] = []
    checked = 0
    allowed = 0
    for scope in policy["scopes"]:
        allowlist = set(scope["engine_allowlist"])
        for relative in scope["files"]:
            path = root / relative
            if not path.is_file():
                errors.append(f"declared capability-policy file is missing: {relative}")
                continue
            checked += 1
            source = path.read_text(encoding="utf-8")
            if ENGINE_REFERENCE.search(source):
                if relative not in allowlist:
                    errors.append(
                        f"{relative}: Engine reference is not allowed in {scope['name']}"
                    )
                else:
                    allowed += 1
            elif relative in allowlist:
                errors.append(
                    f"{relative}: stale Engine allowlist entry in {scope['name']}"
                )
    capability_path = root / policy["capability_module"]
    if capability_path.is_file():
        source = capability_path.read_text(encoding="utf-8")
        for pattern, message in CAPABILITY_ESCAPE_HATCHES:
            if pattern.search(source):
                errors.append(f"{policy['capability_module']}: {message}")
    if errors:
        raise PolicyError("\n".join(errors))
    return {
        "checked_files": checked,
        "declared_engine_adapters": allowed,
        "engine_free_leaf_files": checked - allowed,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=ROOT)
    parser.add_argument("--policy", type=pathlib.Path, default=POLICY_PATH)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = verify(args.root.resolve(), args.policy.resolve())
    except PolicyError as error:
        print(error, file=sys.stderr)
        return 1
    print(
        "Engine capability policy: "
        f"{result['checked_files']} files, "
        f"{result['declared_engine_adapters']} declared adapters, "
        f"{result['engine_free_leaf_files']} engine-free leaves"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
