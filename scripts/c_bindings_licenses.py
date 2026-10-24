#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Retain license files for the selected native C library dependency graph."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PINNED = ROOT / "scripts/c-bindings/licenses"
LEGAL = re.compile(r"(?:^|[-_])(licen[sc]e|copying|copyright|notice)(?:$|[-_.])", re.I)
# These build dependencies supply native objects embedded in their linked owner,
# rather than tooling that runs only on the build host.
NATIVE_BUILD_SOURCES = {"openssl-sys": {"openssl-src"}}


def collect(metadata_path: Path, features: set[str]) -> tuple[dict[str, bytes], list[dict]]:
    metadata = json.loads(metadata_path.read_text())
    packages = {item["id"]: item for item in metadata["packages"]}
    nodes = {item["id"]: item for item in metadata["resolve"]["nodes"]}
    root = next(item["id"] for item in packages.values() if item["name"] == "uqa-ffi")
    enabled = set(nodes[root]["features"]) - {"default"}
    if enabled != features:
        raise ValueError(f"Cargo metadata features {sorted(enabled)} differ from {sorted(features)}")
    selected, pending = set(), [root]
    while pending:
        key = pending.pop()
        if key in selected:
            continue
        selected.add(key)
        native_sources = NATIVE_BUILD_SOURCES.get(packages[key]["name"], set())
        pending.extend(dependency["pkg"] for dependency in nodes[key]["deps"]
                       if any(kind["kind"] is None or (
                           kind["kind"] == "build" and packages[dependency["pkg"]]["name"] in native_sources
                       ) for kind in dependency["dep_kinds"]))
    pinned = {(entry["name"], entry["version"]): entry for entry in json.loads((PINNED / "sources.json").read_text())}
    files, inventory = {}, []
    for key in sorted(selected):
        package = packages[key]
        name, version = package["name"], package["version"]
        directory = Path(package["manifest_path"]).parent
        if key == root:
            continue  # The canonical project license is already installed.
        # Include nested notices from bundled native sources, not just the Rust
        # wrapper's root license. Paths preserve each component's attribution.
        paths = sorted(path for path in directory.rglob("*")
                       if path.is_file() and LEGAL.search(path.name)
                       and path.suffix not in {".rs", ".c", ".h", ".py", ".toml"})
        if package["license_file"]:
            declared = directory / package["license_file"]
            if declared not in paths:
                paths.append(declared)
        payloads = {str(path.relative_to(directory)): path.read_bytes() for path in paths}
        if not payloads:
            supplement = pinned.get((name, version))
            if supplement is None:
                raise ValueError(f"No reviewed license files for {name} {version}")
            vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
            if vcs["git"]["sha1"] != supplement["revision"]:
                raise ValueError(f"Pinned license source differs from {name} {version}")
            for entry in supplement["files"]:
                data = (PINNED / entry["file"]).read_bytes()
                if hashlib.sha256(data).hexdigest() != entry["sha256"]:
                    raise ValueError(f"Pinned license digest changed: {entry['file']}")
                payloads[entry["file"]] = data
        installed = []
        for relative, data in payloads.items():
            destination = f"THIRD-PARTY/crates/{name}-{version}/{relative}"
            files[destination] = data
            installed.append({"path": destination, "sha256": hashlib.sha256(data).hexdigest()})
        inventory.append({"name": name, "version": version, "license": package["license"],
                          "repository": package["repository"], "files": installed})
    # Rust's standard library is linked into staticlib/cdylib outputs but is not
    # a Cargo dependency. Retain the installed toolchain's own runtime inventory.
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    documentation = sysroot / "share/doc/rust"
    copyright_path = documentation / "COPYRIGHT-library.html"
    if not copyright_path.is_file():
        copyright_path = documentation / "COPYRIGHT.html"
    if not copyright_path.is_file():
        raise ValueError("The Rust toolchain is missing its distributed copyright inventory")
    runtime_files = [copyright_path, *sorted((documentation / "licenses").glob("*"))]
    installed = []
    for path in runtime_files:
        if not path.is_file():
            continue
        data = path.read_bytes()
        destination = "THIRD-PARTY/rust-toolchain/" + path.relative_to(documentation).as_posix()
        files[destination] = data
        installed.append({"path": destination, "sha256": hashlib.sha256(data).hexdigest()})
    inventory.append({"name": "rust-standard-library",
                      "version": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                      "license": "See the included Rust copyright inventory",
                      "repository": "https://github.com/rust-lang/rust", "files": installed})
    return files, inventory
