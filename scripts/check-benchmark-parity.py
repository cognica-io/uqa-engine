#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate the pinned Python-to-Rust benchmark parity evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "benchmarks" / "parity" / "manifest.json"
EXPECTED_SOURCE_FILES = {
    f"benchmarks/bench_{name}.py"
    for name in (
        "calibration",
        "compiler",
        "e2e",
        "execution",
        "external_prior",
        "graph",
        "graph_advanced",
        "graph_centrality",
        "multi_field",
        "named_graphs",
        "planner",
        "posting_list",
        "scoring",
        "scoring_advanced",
        "storage",
    )
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
TEST_RE = re.compile(r"^\s*def test_[A-Za-z0-9_]+\s*\(", re.MULTILINE)


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


def verify_rust_surface(surface: object) -> int:
    if not isinstance(surface, dict):
        raise RuntimeError(f"invalid Rust surface: {surface!r}")
    path = safe_repo_path(surface.get("path"))
    try:
        source = path.read_text(encoding="utf-8")
    except OSError as error:
        raise RuntimeError(f"cannot read Rust benchmark {path}: {error}") from error
    tokens = surface.get("tokens")
    if not isinstance(tokens, list) or not tokens:
        raise RuntimeError(f"Rust surface {path} has no evidence tokens")
    if len(tokens) != len(set(tokens)) or not all(isinstance(token, str) and token for token in tokens):
        raise RuntimeError(f"Rust surface {path} has invalid or duplicate evidence tokens")
    missing = [token for token in tokens if token not in source]
    if missing:
        raise RuntimeError(f"Rust benchmark {path} is missing evidence tokens: {missing}")
    return len(tokens)


def verify_source_snapshot(root: pathlib.Path, manifest: dict[str, object]) -> None:
    if not root.is_dir():
        raise RuntimeError(f"Python source root does not exist: {root}")
    expected_commit = manifest["source"]["commit"]
    actual_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if actual_commit != expected_commit:
        raise RuntimeError(
            f"Python source commit differs: expected {expected_commit}, got {actual_commit}"
        )

    for entry in manifest["files"]:
        source_path = root.joinpath(*pathlib.PurePosixPath(entry["path"]).parts)
        try:
            payload = source_path.read_bytes()
        except OSError as error:
            raise RuntimeError(f"cannot read Python benchmark {source_path}: {error}") from error
        digest = hashlib.sha256(payload).hexdigest()
        if digest != entry["sha256"]:
            raise RuntimeError(
                f"Python benchmark hash differs for {entry['path']}: "
                f"expected {entry['sha256']}, got {digest}"
            )
        case_count = len(TEST_RE.findall(payload.decode("utf-8")))
        if case_count != entry["source_case_count"]:
            raise RuntimeError(
                f"Python benchmark case count differs for {entry['path']}: "
                f"expected {entry['source_case_count']}, got {case_count}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--source-root",
        type=pathlib.Path,
        help="optional checkout of the pinned cognica-io/uqa source snapshot",
    )
    args = parser.parse_args()
    manifest = load_manifest()
    if manifest.get("schema_version") != 1:
        raise RuntimeError(f"unsupported parity schema: {manifest.get('schema_version')!r}")
    source = manifest.get("source")
    if not isinstance(source, dict) or not re.fullmatch(r"[0-9a-f]{40}", str(source.get("commit"))):
        raise RuntimeError("benchmark parity source commit must be a full Git object id")

    entries = manifest.get("files")
    if not isinstance(entries, list):
        raise RuntimeError("benchmark parity manifest has no files array")
    paths = [entry.get("path") for entry in entries if isinstance(entry, dict)]
    if len(paths) != len(entries) or len(paths) != len(set(paths)):
        raise RuntimeError("benchmark parity source paths are invalid or duplicated")
    actual_files = set(paths)
    if actual_files != EXPECTED_SOURCE_FILES:
        missing = sorted(EXPECTED_SOURCE_FILES - actual_files)
        extra = sorted(actual_files - EXPECTED_SOURCE_FILES)
        raise RuntimeError(f"benchmark parity file set differs; missing={missing}, extra={extra}")

    surface_count = 0
    case_count = 0
    for entry in entries:
        digest = entry.get("sha256")
        if not isinstance(digest, str) or SHA256_RE.fullmatch(digest) is None:
            raise RuntimeError(f"invalid source SHA-256 for {entry.get('path')}: {digest!r}")
        cases = entry.get("source_case_count")
        if not isinstance(cases, int) or cases <= 0:
            raise RuntimeError(f"invalid source case count for {entry.get('path')}: {cases!r}")
        surfaces = entry.get("rust_surfaces")
        if not isinstance(surfaces, list) or not surfaces:
            raise RuntimeError(f"source benchmark {entry.get('path')} has no Rust mapping")
        case_count += cases
        surface_count += sum(verify_rust_surface(surface) for surface in surfaces)

    additional = manifest.get("additional_rust_coverage", [])
    if not isinstance(additional, list):
        raise RuntimeError("additional_rust_coverage must be an array")
    surface_count += sum(verify_rust_surface(surface) for surface in additional)
    if args.source_root is not None:
        verify_source_snapshot(args.source_root.resolve(), manifest)

    source_marker = " with source snapshot" if args.source_root is not None else ""
    print(
        f"Benchmark parity OK: {len(entries)} Python files, {case_count} named cases, "
        f"{surface_count} Rust evidence tokens{source_marker}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
