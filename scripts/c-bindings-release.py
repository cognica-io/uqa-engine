#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Package and verify the native C library's installed, target-specific assets."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin",
           "aarch64-apple-darwin", "x86_64-pc-windows-msvc")


def version() -> str:
    versions = re.findall(r'^version = "([^"\n]+)"$', (ROOT / "Cargo.toml").read_text(), re.M)
    if len(versions) != 1:
        raise ValueError("Expected one workspace package version")
    return versions[0]


def asset_name(target: str) -> str:
    if target not in TARGETS:
        raise ValueError(f"Unsupported C release target: {target}")
    return f"uqa-c-{version()}-{target}" + (".zip" if "windows" in target else ".tgz")


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return stream_digest(stream)


def stream_digest(stream) -> str:
    checksum = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        checksum.update(chunk)
    return checksum.hexdigest()


def safe_path(name: str) -> None:
    path = PurePosixPath(name)
    if not name or name == "." or path.as_posix() != name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"Unsafe archive path: {name!r}")


def validate_inventory(entries: dict[str, dict], manifest: dict, target: str, revision: str) -> None:
    if manifest["schema_version"] != 1 or manifest["abi_major"] != 1:
        raise ValueError("Unsupported C package schema or ABI")
    if (manifest["version"], manifest["target"], manifest["source_revision"]) != (version(), target, revision):
        raise ValueError("C package source, target or version differs from the release")
    # Distributed packages retain the documented default dictionaries. Optional
    # FDWs/export are source build selections with stable unsupported symbols.
    if manifest["features"] != ["kuromoji", "nori"]:
        raise ValueError("C release feature inventory differs from the reviewed default")
    if not manifest.get("native_dynamic_libraries"):
        raise ValueError("Missing native dynamic-library dependency inventory")
    spec = importlib.util.spec_from_file_location("c_metadata", ROOT / "scripts/c-bindings-metadata.py")
    metadata = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(metadata)
    exported, deferred = metadata.symbols()
    if manifest["exports"] != exported or manifest["declaration_only"] != {"uqa_log.h": deferred}:
        raise ValueError("C package function inventory differs from the release headers")
    required = {"share/uqa/uqa-build.json", "lib/pkgconfig/uqa.pc", "lib/pkgconfig/uqa-static.pc",
                "lib/cmake/UQA/UQAConfig.cmake", "lib/cmake/UQA/UQAConfigVersion.cmake",
                "lib/cmake/UQA/UQATargets.cmake"}
    required |= {f"include/uqa/{path.name}" for path in (ROOT / "crates/uqa-ffi/include/uqa").glob("*.h")}
    if "windows" in target:
        required |= {"bin/uqa.dll", "lib/uqa.lib", "lib/uqa_static.lib"}
    else:
        required |= {"lib/libuqa.a", "lib/libuqa.dylib" if "apple" in target else "lib/libuqa.so"}
    if not required <= entries.keys():
        raise ValueError(f"Incomplete C package inventory: {sorted(required-entries.keys())}")
    for name, entry in entries.items():
        safe_path(name)
        if "link" in entry:
            safe_path(entry["link"])
            destination = str(PurePosixPath(name).parent / entry["link"])
            if destination not in entries:
                raise ValueError(f"Dangling archive link: {name}")
            visited = {name}
            while "link" in entries[destination]:
                if destination in visited:
                    raise ValueError(f"Cyclic archive link: {name}")
                visited.add(destination)
                link = entries[destination]["link"]
                safe_path(link)
                destination = str(PurePosixPath(destination).parent / link)
                if destination not in entries:
                    raise ValueError(f"Dangling archive link: {name}")
    for package in manifest["dependencies"]:
        for item in package["files"]:
            path = "share/uqa/licenses/" + item["path"]
            if entries.get(path, {}).get("sha256") != item["sha256"]:
                raise ValueError(f"Missing or altered dependency license: {path}")


def package(prefix: Path, target: str, output: Path) -> None:
    manifest = json.loads((prefix / "share/uqa/uqa-build.json").read_text())
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    paths = sorted(path for path in prefix.rglob("*") if path.is_file() or path.is_symlink())
    entries = {}
    for path in paths:
        name = path.relative_to(prefix).as_posix()
        if path.is_symlink():
            entries[name] = {"link": path.readlink().as_posix()}
        else:
            entries[name] = {"sha256": digest(path), "size": path.stat().st_size}
    validate_inventory(entries, manifest, target, revision)
    inventory = (json.dumps(entries, indent=2, sort_keys=True) + "\n").encode()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / asset_name(target)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as writer:
            for path in paths:
                if path.is_symlink():
                    raise ValueError("Windows packages must contain regular files")
                writer.write(path, path.relative_to(prefix).as_posix())
            writer.writestr("uqa-package.json", inventory)
    else:
        with tarfile.open(archive, "w:gz") as writer:
            for path in paths:
                writer.add(path, arcname=path.relative_to(prefix).as_posix(), recursive=False)
            entry = tarfile.TarInfo("uqa-package.json")
            entry.size = len(inventory)
            entry.mode = 0o644
            writer.addfile(entry, io.BytesIO(inventory))
    archive.with_name(archive.name + ".sha256").write_text(f"{digest(archive)}  {archive.name}\n")
    verify(target, output)


def verify(target: str, directory: Path) -> None:
    archive = directory / asset_name(target)
    checksum = archive.with_name(archive.name + ".sha256").read_text()
    if checksum != f"{digest(archive)}  {archive.name}\n":
        raise ValueError(f"C package checksum mismatch: {archive.name}")
    entries, retained = {}, {}

    def record(name: str, stream, size: int) -> None:
        safe_path(name)
        if name in entries:
            raise ValueError(f"Duplicate archive entry: {name}")
        if name in {"uqa-package.json", "share/uqa/uqa-build.json"} or name.startswith("include/"):
            data = stream.read()
            retained[name] = data
            checksum = hashlib.sha256(data).hexdigest()
        else:
            checksum = stream_digest(stream)
        entries[name] = {"sha256": checksum, "size": size}

    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as reader:
            for member in reader.infolist():
                if member.is_dir() or (member.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError("Unexpected Windows archive entry")
                with reader.open(member) as stream:
                    record(member.filename, stream, member.file_size)
    else:
        with tarfile.open(archive) as reader:
            for member in reader:
                if member.issym():
                    safe_path(member.name)
                    if member.name in entries:
                        raise ValueError(f"Duplicate archive entry: {member.name}")
                    entries[member.name] = {"link": member.linkname}
                elif member.isfile():
                    with reader.extractfile(member) as stream:
                        record(member.name, stream, member.size)
                else:
                    raise ValueError(f"Unexpected archive entry: {member.name}")
    inventory = json.loads(retained["uqa-package.json"])
    del entries["uqa-package.json"]
    if entries != inventory:
        raise ValueError("C archive contents differ from its file inventory")
    manifest = json.loads(retained["share/uqa/uqa-build.json"])
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    validate_inventory(entries, manifest, target, revision)
    for path in (ROOT / "crates/uqa-ffi/include/uqa").glob("*.h"):
        if retained[f"include/uqa/{path.name}"] != path.read_bytes():
            raise ValueError(f"Packaged header differs from release source: {path.name}")
    print(f"Verified C release identity, complete inventory and digests: {archive.name}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("package", "verify"))
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--prefix", type=Path)
    args = parser.parse_args()
    if args.command == "package":
        if args.target is None or args.prefix is None:
            parser.error("package requires --target and --prefix")
        package(args.prefix, args.target, args.directory)
    else:
        for target in (args.target,) if args.target else TARGETS:
            verify(target, args.directory)


if __name__ == "__main__":
    main()
