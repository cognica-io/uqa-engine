#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Checked MeCab source extraction and exact generated-resource validation for Lucene tools."""

from pathlib import Path, PurePosixPath
import shutil
import tarfile
import tempfile
from urllib.request import urlopen

from lucene_runtime import sha256

DEFINITIONS = {"char.def", "unk.def", "matrix.def"}


def prepare_source(manifest, cache, offline):
    source = manifest["dictionary_source"]
    archive = cache / (source["name"] + ".tar.gz")
    if not archive.exists():
        if offline:
            raise RuntimeError(f"Missing cached dictionary source: {archive}")
        with tempfile.TemporaryDirectory(prefix="lucene-source-", dir=cache) as temporary:
            staged = Path(temporary) / archive.name
            with urlopen(source["url"], timeout=60) as download, staged.open("wb") as output:
                shutil.copyfileobj(download, output)
            if sha256(staged) != source["sha256"]:
                raise RuntimeError("Downloaded dictionary source checksum mismatch")
            staged.replace(archive)
    if sha256(archive) != source["sha256"]:
        raise RuntimeError(f"Cached dictionary source checksum mismatch: {archive}")
    return archive


def extract_inputs(archive, source_name, directory):
    """Copy only the top-level inputs read by DictionaryBuilder; never unpack executable archive content."""
    names = set()
    with tarfile.open(archive, "r:gz") as source:
        for entry in source:
            path = PurePosixPath(entry.name)
            if path.is_absolute() or ".." in path.parts:
                raise RuntimeError(f"Invalid dictionary archive path: {entry.name}")
            if len(path.parts) != 2 or path.parts[0] != source_name:
                continue
            name = path.name
            if not name.endswith(".csv") and name not in DEFINITIONS:
                continue
            if not entry.isfile() or name in names:
                raise RuntimeError(f"Invalid or duplicate dictionary input: {entry.name}")
            names.add(name)
            with source.extractfile(entry) as stream, (directory / name).open("wb") as output:
                shutil.copyfileobj(stream, output)
    if not DEFINITIONS.issubset(names) or not any(name.endswith(".csv") for name in names):
        raise RuntimeError("Dictionary archive is missing CSV or definition inputs")
    return inventory(directory)


def inventory(directory):
    return [
        {"path": path.relative_to(directory).as_posix(), "bytes": path.stat().st_size, "sha256": sha256(path)}
        for path in sorted(directory.rglob("*")) if path.is_file()
    ]


def check_resources(directory, expected):
    actual = inventory(directory)
    expected = sorted(expected, key=lambda item: item["path"])
    if actual != expected:
        actual_by_path = {item["path"]: item for item in actual}
        expected_by_path = {item["path"]: item for item in expected}
        changed = [name for name in sorted(actual_by_path.keys() | expected_by_path.keys())
                   if actual_by_path.get(name) != expected_by_path.get(name)]
        raise RuntimeError("Regenerated resources differ from the pinned jar: " + ", ".join(changed))
    return actual
