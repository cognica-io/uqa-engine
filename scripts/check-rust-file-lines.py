#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Enforce the physical-line ceiling for hand-maintained Rust source files."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import sys
from collections import defaultdict
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "scripts" / "rust-file-line-policy.json"
REPORT_THRESHOLDS = (1000, 1200, 1350, 1400, 1450, 1501)
POLICY_FIELDS = {"schema_version", "line_limit", "excluded_roots"}


class PolicyError(RuntimeError):
    """The line policy or current tree violates the hard ceiling."""


def physical_line_count(path: pathlib.Path) -> int:
    data = path.read_bytes()
    if not data:
        return 0
    return data.count(b"\n") + (0 if data.endswith(b"\n") else 1)


def normalized_relative_path(value: object, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise PolicyError(f"{field} must be a non-empty relative path")
    path = pathlib.PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or path.as_posix() != value:
        raise PolicyError(f"{field} must be a normalized relative path: {value!r}")
    return value


def load_policy(path: pathlib.Path) -> dict[str, Any]:
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyError(f"cannot read Rust line policy {path}: {error}") from error
    if not isinstance(policy, dict) or set(policy) != POLICY_FIELDS:
        raise PolicyError(f"Rust line policy must contain exactly {sorted(POLICY_FIELDS)}")
    if policy.get("schema_version") != 2:
        raise PolicyError("Rust line policy must use schema_version 2")
    limit = policy.get("line_limit")
    if not isinstance(limit, int) or isinstance(limit, bool) or limit <= 0:
        raise PolicyError("line_limit must be a positive integer")
    excluded = policy.get("excluded_roots")
    if not isinstance(excluded, list):
        raise PolicyError("excluded_roots must be a list")
    policy["excluded_roots"] = [
        normalized_relative_path(value, "excluded_roots entry") for value in excluded
    ]
    if len(set(policy["excluded_roots"])) != len(policy["excluded_roots"]):
        raise PolicyError("excluded_roots contains duplicates")
    return policy


def under_excluded_root(path: str, excluded_roots: list[str]) -> bool:
    return any(path == root or path.startswith(f"{root}/") for root in excluded_roots)


def rust_file_lines(root: pathlib.Path, excluded_roots: list[str]) -> dict[str, int]:
    result: dict[str, int] = {}
    for directory, directories, filenames in os.walk(root):
        directory_path = pathlib.Path(directory)
        relative_directory = directory_path.relative_to(root)
        retained = []
        for name in directories:
            if name == ".git" or name == "target":
                continue
            candidate = (relative_directory / name).as_posix()
            if under_excluded_root(candidate, excluded_roots):
                continue
            retained.append(name)
        directories[:] = retained
        for filename in filenames:
            if not filename.endswith(".rs"):
                continue
            path = directory_path / filename
            relative = path.relative_to(root).as_posix()
            if not under_excluded_root(relative, excluded_roots):
                result[relative] = physical_line_count(path)
    return dict(sorted(result.items()))


def text_contains(path: pathlib.Path, pattern: str) -> bool:
    return re.search(pattern, path.read_text(encoding="utf-8")) is not None


def report(root: pathlib.Path, lines: dict[str, int], limit: int) -> dict[str, Any]:
    per_crate: dict[str, dict[str, int]] = defaultdict(lambda: {"files": 0, "physical_lines": 0})
    for path, count in lines.items():
        parts = pathlib.PurePosixPath(path).parts
        if len(parts) >= 3 and parts[0] == "crates":
            values = per_crate[parts[1]]
            values["files"] += 1
            values["physical_lines"] += count

    engine_paths = [path for path in lines if path.startswith("crates/uqa-engine/src/")]
    sql_paths = [
        path
        for path in lines
        if path == "crates/uqa-engine/src/sql.rs"
        or path.startswith("crates/uqa-engine/src/sql/")
    ]
    root_structural_allowances = []
    for path in (
        "crates/uqa-engine/src/sql.rs",
        "crates/uqa-execution/src/lib.rs",
        "crates/uqa-sql/src/lib.rs",
    ):
        absolute = root / path
        if absolute.exists() and text_contains(
            absolute,
            r"(?s)#!\[allow\([^]]*clippy::(?:too_many_lines|too_many_arguments|type_complexity|large_enum_variant)",
        ):
            root_structural_allowances.append(path)

    largest_path, largest_lines = max(lines.items(), key=lambda item: item[1])
    return {
        "line_limit": limit,
        "rust_files": len(lines),
        "physical_lines": sum(lines.values()),
        "files_at_or_above_limit": sum(count >= limit for count in lines.values()),
        "threshold_counts": {
            str(threshold): sum(count >= threshold for count in lines.values())
            for threshold in REPORT_THRESHOLDS
        },
        "largest_file": {"path": largest_path, "physical_lines": largest_lines},
        "per_crate": dict(sorted(per_crate.items())),
        "uqa_engine_src_sql": {
            "files": len(sql_paths),
            "physical_lines": sum(lines[path] for path in sql_paths),
        },
        "engine_coupling": {
            "src_files": len(engine_paths),
            "files_mentioning_Engine": sum(
                text_contains(root / path, r"\bEngine\b") for path in engine_paths
            ),
            "files_containing_literal_impl_Engine": sum(
                "impl Engine" in (root / path).read_text(encoding="utf-8")
                for path in engine_paths
            ),
            "sql_files_mentioning_Engine": sum(
                text_contains(root / path, r"\bEngine\b") for path in sql_paths
            ),
            "sql_files_importing_parent": sum(
                text_contains(root / path, r"(?m)^use super(?:::|\{)") for path in sql_paths
            ),
            "sql_files_using_parent_glob": sum(
                text_contains(root / path, r"(?m)^use super::\*;") for path in sql_paths
            ),
        },
        "root_structural_lint_allowances": root_structural_allowances,
    }


def verify(root: pathlib.Path, policy_path: pathlib.Path) -> dict[str, Any]:
    policy = load_policy(policy_path)
    limit = policy["line_limit"]
    lines = rust_file_lines(root, policy["excluded_roots"])
    errors = [
        f"Rust file reaches or exceeds the {limit}-line ceiling: {path} ({current})"
        for path, current in lines.items()
        if current >= limit
    ]

    if errors:
        raise PolicyError("\n".join(errors))
    return report(root, lines, limit)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=ROOT)
    parser.add_argument("--policy", type=pathlib.Path, default=POLICY_PATH)
    parser.add_argument(
        "--report",
        action="store_true",
        help="print the reproducible physical-line and coupling report as JSON",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = verify(args.root.resolve(), args.policy.resolve())
    except PolicyError as error:
        print(error, file=sys.stderr)
        return 1
    if args.report:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        largest = result["largest_file"]
        print(
            "Rust file hard ceiling OK "
            f"(all files below {result['line_limit']} lines; largest {largest['path']} "
            f"at {largest['physical_lines']})"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
