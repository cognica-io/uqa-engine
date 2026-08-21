#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Import or verify the pinned PostgreSQL 18 pg_query.rs tree as uqa-pg-query."""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import shutil
import stat
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
DEST = ROOT / "crates" / "uqa-pg-query"
CHECKSUMS = DEST / "SHA256SUMS"
PG_QUERY_REV = "516b3a03fed42e606ce01bc8b5a864a1698c210d"
LIBPG_QUERY_REV = "898cd71c96375d6d4219916996701571dbe2b239"
PG_QUERY_REPO = "https://github.com/jaepil/pg_query.rs"
LIBPG_QUERY_REPO = "https://github.com/jaepil/libpg_query"

OWNED_FILES = {
    "Cargo.toml",
    "UPSTREAM.md",
    "SHA256SUMS",
    "tests/integration.rs",
}

IMPORT_ROOT_FILES = (
    "build.rs",
    "LICENSE",
    "README.md",
)

LIBPG_QUERY_ROOT_FILES = (
    "Makefile",
    "pg_query.h",
    "postgres_deparse.h",
)

LIBPG_QUERY_PROTOBUF_FILES = (
    "pg_query.pb-c.c",
    "pg_query.pb-c.h",
    "pg_query.proto",
)


def run(command: list[str], cwd: pathlib.Path | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def iter_files(root: pathlib.Path) -> list[pathlib.Path]:
    files = [path for path in root.rglob("*") if path.is_file()]
    files.sort()
    return files


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_checksums(files: list[tuple[str, pathlib.Path]]) -> None:
    lines = [f"{sha256_file(path)}  {relative}\n" for relative, path in files]
    CHECKSUMS.write_text("".join(lines), encoding="utf-8")


def parse_checksums() -> list[tuple[str, str]]:
    try:
        text = CHECKSUMS.read_text(encoding="utf-8")
    except OSError as error:
        raise RuntimeError(f"cannot read {CHECKSUMS}: {error}") from error
    entries: list[tuple[str, str]] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        if not line:
            continue
        try:
            digest, relative = line.split("  ", 1)
        except ValueError as error:
            raise RuntimeError(f"{CHECKSUMS}:{line_number} is not a checksum line") from error
        entries.append((digest, relative))
    return entries


def cargo_checkout() -> pathlib.Path | None:
    cargo_home = pathlib.Path(os.environ.get("CARGO_HOME", pathlib.Path.home() / ".cargo"))
    checkouts = cargo_home / "git" / "checkouts"
    if not checkouts.is_dir():
        return None
    for candidate in checkouts.glob("pg_query.rs-*/516b3a0"):
        if not (candidate / "build.rs").is_file():
            continue
        try:
            revision = run(["git", "rev-parse", "HEAD"], cwd=candidate)
            lib_revision = run(["git", "rev-parse", "HEAD"], cwd=candidate / "libpg_query")
        except (OSError, subprocess.CalledProcessError):
            continue
        if revision == PG_QUERY_REV and lib_revision == LIBPG_QUERY_REV:
            return candidate
    return None


def clone_sources(work_dir: pathlib.Path) -> pathlib.Path:
    source = work_dir / "pg_query.rs"
    run(["git", "clone", "--no-checkout", PG_QUERY_REPO, str(source)])
    run(["git", "checkout", "--detach", PG_QUERY_REV], cwd=source)
    run(["git", "submodule", "update", "--init", "libpg_query"], cwd=source)
    lib_revision = run(["git", "rev-parse", "HEAD"], cwd=source / "libpg_query")
    if lib_revision != LIBPG_QUERY_REV:
        raise RuntimeError(
            f"libpg_query HEAD is {lib_revision}, expected {LIBPG_QUERY_REV}"
        )
    return source


def clear_imported_files() -> None:
    if not DEST.is_dir():
        return
    owned_roots = {pathlib.PurePosixPath(relative).parts[0] for relative in OWNED_FILES}
    for path in DEST.iterdir():
        if path.name in owned_roots:
            continue
        if path.is_dir():
            shutil.rmtree(path)
        else:
            path.unlink()


def copy_file(source: pathlib.Path, destination: pathlib.Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    mode = destination.stat().st_mode
    destination.chmod(mode | stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)


def import_tree(source: pathlib.Path) -> list[tuple[str, pathlib.Path]]:
    imported: list[tuple[str, pathlib.Path]] = []

    def add(relative: str, src: pathlib.Path) -> None:
        dest = DEST / relative
        copy_file(src, dest)
        imported.append((relative, dest))

    for name in IMPORT_ROOT_FILES:
        add(name, source / name)
    add("LIBPG_QUERY-LICENSE", source / "libpg_query" / "LICENSE")
    for name in LIBPG_QUERY_ROOT_FILES:
        add(f"libpg_query/{name}", source / "libpg_query" / name)
    for src in iter_files(source / "libpg_query" / "src"):
        relative = src.relative_to(source).as_posix()
        add(relative, src)
    for src in iter_files(source / "libpg_query" / "vendor"):
        relative = src.relative_to(source).as_posix()
        add(relative, src)
    for name in LIBPG_QUERY_PROTOBUF_FILES:
        add(f"libpg_query/protobuf/{name}", source / "libpg_query" / "protobuf" / name)
    for src in iter_files(source / "src"):
        if src.suffix != ".rs":
            continue
        relative = src.relative_to(source).as_posix()
        add(relative, src)
    imported.sort(key=lambda item: item[0])
    return imported


def check_tree() -> None:
    expected = parse_checksums()
    expected_map = {relative: digest for digest, relative in expected}
    actual_files = []
    for path in iter_files(DEST):
        relative = path.relative_to(DEST).as_posix()
        if relative in OWNED_FILES:
            continue
        actual_files.append(relative)
    missing = sorted(set(expected_map) - set(actual_files))
    extra = sorted(set(actual_files) - set(expected_map))
    mismatched = []
    for relative in sorted(set(expected_map) & set(actual_files)):
        digest = sha256_file(DEST / relative)
        if digest != expected_map[relative]:
            mismatched.append(relative)
    if missing or extra or mismatched:
        details = []
        if missing:
            details.append("missing: " + ", ".join(missing[:20]))
        if extra:
            details.append("extra: " + ", ".join(extra[:20]))
        if mismatched:
            details.append("changed: " + ", ".join(mismatched[:20]))
        raise RuntimeError("uqa-pg-query import drifted from SHA256SUMS (" + "; ".join(details) + ")")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the imported tree against SHA256SUMS",
    )
    parser.add_argument(
        "--source",
        type=pathlib.Path,
        help="existing checkout of jaepil/pg_query.rs at the pinned revision",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.check:
        check_tree()
        print(f"uqa-pg-query import OK: {len(parse_checksums())} files")
        return 0

    source = args.source
    work_dir: pathlib.Path | None = None
    if source is None:
        source = cargo_checkout()
    if source is None:
        work_dir = pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / "uqa-pg-query-sync"
        if work_dir.exists():
            shutil.rmtree(work_dir)
        work_dir.mkdir(parents=True)
        source = clone_sources(work_dir)
    DEST.mkdir(parents=True, exist_ok=True)
    clear_imported_files()
    imported = import_tree(source)
    write_checksums(imported)
    print(f"Imported {len(imported)} files into {DEST}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
