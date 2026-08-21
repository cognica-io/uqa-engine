#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate licensing files in release sources and built archives."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
import tarfile
import tomllib
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
LEGAL_FILES = (
    "LICENSE",
    "LICENSING.md",
    "LICENSES/UQA-FOSS-EXCEPTION-1.0.txt",
    "LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt",
)
AGPL_NOTICE = "LICENSE-NOTICE.md"
MIT_PARSER_FILES = (
    "LICENSE",
    "LIBPG_QUERY-LICENSE",
)
NPM_PACKAGES = (
    ROOT / "crates" / "uqa-node",
    ROOT / "crates" / "uqa-wasm" / "js",
)
MIT_PARSER_CRATE = "uqa-pg-query"


def canonical_payloads() -> dict[str, bytes]:
    payloads: dict[str, bytes] = {}
    for relative in LEGAL_FILES:
        path = ROOT / relative
        try:
            payloads[relative] = path.read_bytes()
        except OSError as error:
            raise RuntimeError(f"cannot read canonical legal file {path}: {error}") from error
    return payloads


def check_npm_sources(payloads: dict[str, bytes]) -> None:
    required_files = {"LICENSE-NOTICE.md", *LEGAL_FILES}
    for package_root in NPM_PACKAGES:
        manifest_path = package_root / "package.json"
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"cannot read {manifest_path}: {error}") from error
        declared = set(manifest.get("files", []))
        missing = sorted(required_files - declared)
        if missing:
            raise RuntimeError(f"{manifest_path} omits release legal files: {missing}")
        for relative, expected in payloads.items():
            package_path = package_root / relative
            try:
                actual = package_path.read_bytes()
            except OSError as error:
                raise RuntimeError(f"cannot read {package_path}: {error}") from error
            if actual != expected:
                raise RuntimeError(f"npm legal copy differs from canonical file: {package_path}")


def workspace_packages() -> list[dict[str, object]]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    members = set(metadata["workspace_members"])
    return [package for package in metadata["packages"] if package["id"] in members]


def is_publishable(package: dict[str, object]) -> bool:
    publish = package.get("publish")
    if publish is None:
        return True
    if publish is False:
        return False
    if isinstance(publish, list) and len(publish) == 0:
        return False
    return True


def check_cargo_sources(payloads: dict[str, bytes]) -> None:
    notice = (ROOT / "crates" / "uqa-node" / AGPL_NOTICE).read_bytes()
    for package in workspace_packages():
        name = str(package["name"])
        if not is_publishable(package):
            continue
        crate_root = pathlib.Path(str(package["manifest_path"])).parent
        if name == MIT_PARSER_CRATE:
            for relative in MIT_PARSER_FILES:
                path = crate_root / relative
                try:
                    path.read_bytes()
                except OSError as error:
                    raise RuntimeError(f"cannot read {path}: {error}") from error
            agpl_license = payloads["LICENSE"]
            parser_license = (crate_root / "LICENSE").read_bytes()
            if parser_license == agpl_license:
                raise RuntimeError(f"{crate_root / 'LICENSE'} must remain the MIT wrapper license")
            continue
        for relative, expected in payloads.items():
            path = crate_root / relative
            try:
                actual = path.read_bytes()
            except OSError as error:
                raise RuntimeError(f"cannot read {path}: {error}") from error
            if actual != expected:
                raise RuntimeError(f"crate legal copy differs from canonical file: {path}")
        notice_path = crate_root / AGPL_NOTICE
        try:
            actual_notice = notice_path.read_bytes()
        except OSError as error:
            raise RuntimeError(f"cannot read {notice_path}: {error}") from error
        if actual_notice != notice:
            raise RuntimeError(f"crate legal copy differs from canonical file: {notice_path}")


def check_maturin_sources() -> None:
    pyproject_path = ROOT / "pyproject.toml"
    try:
        pyproject = tomllib.loads(pyproject_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise RuntimeError(f"cannot read {pyproject_path}: {error}") from error
    project = pyproject.get("project", {})
    if project.get("license") != "AGPL-3.0-only":
        raise RuntimeError("Python package must declare the AGPL-3.0-only SPDX license")
    declared = project.get("license-files", [])
    required = {"LICENSE", "LICENSING.md", "LICENSES/*.txt"}
    declared_paths = (
        {value for value in declared if isinstance(value, str)}
        if isinstance(declared, list)
        else set()
    )
    if not required.issubset(declared_paths):
        raise RuntimeError(f"Python package license-files must include {sorted(required)}")


def archive_members(path: pathlib.Path) -> dict[str, bytes]:
    if path.suffix == ".whl":
        try:
            with zipfile.ZipFile(path) as archive:
                return {
                    member.filename: archive.read(member)
                    for member in archive.infolist()
                    if not member.is_dir()
                }
        except (OSError, zipfile.BadZipFile) as error:
            raise RuntimeError(f"cannot read wheel {path}: {error}") from error
    if path.name.endswith((".tar.gz", ".tgz", ".crate")):
        try:
            with tarfile.open(path, "r:gz") as archive:
                return {
                    member.name: extracted.read()
                    for member in archive.getmembers()
                    if member.isfile()
                    and (extracted := archive.extractfile(member)) is not None
                }
        except (OSError, tarfile.TarError) as error:
            raise RuntimeError(f"cannot read tar archive {path}: {error}") from error
    raise RuntimeError(f"unsupported release archive: {path}")


def matching_members(members: dict[str, bytes], relative: str) -> list[tuple[str, bytes]]:
    suffix = f"/{relative}"
    return [
        (name, payload)
        for name, payload in members.items()
        if name == relative or name.endswith(suffix)
    ]


def check_archive(path: pathlib.Path, payloads: dict[str, bytes]) -> None:
    members = archive_members(path)
    if path.name.startswith(f"{MIT_PARSER_CRATE}-") and path.name.endswith(".crate"):
        for relative in MIT_PARSER_FILES:
            if not matching_members(members, relative):
                raise RuntimeError(f"{path} omits {relative}")
        if matching_members(members, "LICENSING.md"):
            raise RuntimeError(f"{path} must not ship the AGPL licensing policy as its own license")
        return
    for relative, expected in payloads.items():
        matches = matching_members(members, relative)
        if not matches:
            raise RuntimeError(f"{path} omits {relative}")
        if not any(payload == expected for _, payload in matches):
            names = [name for name, _ in matches]
            raise RuntimeError(f"{path} contains a noncanonical {relative}: {names}")
    if path.name.endswith((".tgz", ".crate")) and not matching_members(
        members, AGPL_NOTICE
    ):
        raise RuntimeError(f"{path} omits {AGPL_NOTICE}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("archives", nargs="*", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    payloads = canonical_payloads()
    check_npm_sources(payloads)
    check_maturin_sources()
    check_cargo_sources(payloads)
    for archive in args.archives:
        check_archive(archive.resolve(), payloads)
    archive_suffix = f" and {len(args.archives)} archive(s)" if args.archives else ""
    print(f"Release license contract OK: canonical sources{archive_suffix}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
