#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate the Rust benchmark inventory and its semantic evidence tokens."""

from __future__ import annotations

import json
import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "benchmarks" / "coverage" / "manifest.json"
BENCHMARK_GLOB = "crates/*/benches/*.rs"


def load_manifest() -> dict[str, object]:
    try:
        return json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read {MANIFEST_PATH}: {error}") from error


def safe_repo_path(value: object) -> pathlib.Path:
    if not isinstance(value, str) or not value:
        raise RuntimeError(f"invalid repository-relative path: {value!r}")
    relative = pathlib.PurePosixPath(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise RuntimeError(f"path escapes repository: {value!r}")
    return ROOT.joinpath(*relative.parts)


def benchmark_entrypoints() -> set[str]:
    return {
        path.relative_to(ROOT).as_posix()
        for path in ROOT.glob(BENCHMARK_GLOB)
        if path.is_file()
    }


def verify_surface(surface: object) -> int:
    if not isinstance(surface, dict):
        raise RuntimeError(f"invalid benchmark surface: {surface!r}")
    path = safe_repo_path(surface.get("path"))
    try:
        source = path.read_text(encoding="utf-8")
    except OSError as error:
        raise RuntimeError(f"cannot read Rust benchmark {path}: {error}") from error

    tokens = surface.get("tokens")
    if not isinstance(tokens, list) or not tokens:
        raise RuntimeError(f"benchmark surface {path} has no evidence tokens")
    if len(tokens) != len(set(tokens)) or not all(
        isinstance(token, str) and token for token in tokens
    ):
        raise RuntimeError(f"benchmark surface {path} has invalid or duplicate tokens")

    missing = [token for token in tokens if token not in source]
    if missing:
        raise RuntimeError(f"Rust benchmark {path} is missing evidence tokens: {missing}")
    return len(tokens)


def main() -> int:
    manifest = load_manifest()
    if manifest.get("schema_version") != 1:
        raise RuntimeError(
            f"unsupported benchmark coverage schema: {manifest.get('schema_version')!r}"
        )

    surfaces = manifest.get("surfaces")
    if not isinstance(surfaces, list) or not surfaces:
        raise RuntimeError("benchmark coverage manifest has no surfaces array")
    paths = [surface.get("path") for surface in surfaces if isinstance(surface, dict)]
    if len(paths) != len(surfaces) or len(paths) != len(set(paths)):
        raise RuntimeError("benchmark coverage paths are invalid or duplicated")

    declared = set(paths)
    actual = benchmark_entrypoints()
    if declared != actual:
        missing = sorted(actual - declared)
        stale = sorted(declared - actual)
        raise RuntimeError(
            f"benchmark entrypoint inventory differs; missing={missing}, stale={stale}"
        )

    token_count = sum(verify_surface(surface) for surface in surfaces)
    print(
        f"Benchmark coverage OK: {len(surfaces)} Rust entrypoints, "
        f"{token_count} semantic evidence tokens"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
