#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Resolve a tested registry package and its archive checksum through Cargo."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import subprocess


def registry_package_checksum(manifest: Path, name: str, version: str) -> str:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--manifest-path", str(manifest)],
        text=True,
        encoding="utf-8",
    ))
    packages = [
        package for package in metadata["packages"]
        if package["name"] == name and (package.get("source") or "").startswith("registry+")
    ]
    if len(packages) != 1 or packages[0]["version"] != version:
        found = [(package["name"], package["version"]) for package in packages]
        raise RuntimeError(f"expected one registry package {name} {version}, found {found}")
    package_manifest = Path(packages[0]["manifest_path"])
    checksum_file = package_manifest.with_name(".cargo-checksum.json")
    if checksum_file.is_file():
        # Cargo's directory source includes the original archive's checksum.
        checksum = json.loads(checksum_file.read_text(encoding="utf-8")).get("package")
    else:
        # Registry sources are unpacked beside the cache of downloaded archives.
        parents = package_manifest.parents
        if len(parents) < 4 or parents[2].name != "src" or parents[3].name != "registry":
            raise RuntimeError(f"cannot locate the registry archive for {package_manifest}")
        archive = parents[3] / "cache" / parents[1].name / f"{name}-{version}.crate"
        digest = hashlib.sha256()
        with archive.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        checksum = digest.hexdigest()
    if not isinstance(checksum, str) or len(checksum) != 64 or any(character not in "0123456789abcdef" for character in checksum):
        raise RuntimeError(f"missing or invalid registry archive checksum for {name} {version}")
    return checksum
