#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Rebuild the pinned MeCab CSV source in Docker and require exact Lucene resource bytes."""

import argparse
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.request import urlopen

from reference_runtime import ROOT, docker_command, prepare_jars, sha256, verify_dictionary_resources


MANIFEST_PATH = ROOT / "csv_manifest.json"
ENTRYPOINT = "NoriDictionary.java"
DEFINITIONS = {"char.def", "unk.def", "matrix.def"}
RUNTIME_FIELDS = ("java_version", "java_runtime_version", "java_vendor")


def canonical(value):
    return json.dumps(value, ensure_ascii=False, indent=2) + "\n"


def prepare_source(manifest, cache, offline):
    source = manifest["dictionary_source"]
    archive = cache / (source["name"] + ".tar.gz")
    if not archive.exists():
        if offline:
            raise RuntimeError(f"Missing cached dictionary source: {archive}")
        with tempfile.TemporaryDirectory(prefix="nori-source-", dir=cache) as temporary:
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


def provenance(manifest, source, inputs):
    return {
        "format": "uqa-nori-csv-regeneration",
        "format_version": 1,
        "reference_manifest_sha256": sha256(ROOT / "manifest.json"),
        "builder_source_sha256": sha256(ROOT / ENTRYPOINT),
        "source_archive": {**manifest["dictionary_source"], "bytes": source.stat().st_size},
        "builder": {"class": "org.apache.lucene.analysis.ko.dict.DictionaryBuilder",
                    "encoding": "utf-8", "normalize_entries": False, "java_heap": "1g"},
        "runtime": manifest["runtime"],
        "inputs": inputs,
        "resources": sorted(manifest["dictionary_resources"], key=lambda item: item["path"]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New directory for verified resources and provenance")
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-nori-reference-jars")
    parser.add_argument("--offline", action="store_true", help="Require cached jars, source archive, and Docker image")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    parser.add_argument("--write-manifest", action="store_true", help="Record reviewed provenance only after exact jar-resource equality")
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        raise RuntimeError(f"Output already exists; select a new directory: {output}")
    expected = None if args.write_manifest else json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if expected is not None and (
        expected["reference_manifest_sha256"] != sha256(ROOT / "manifest.json")
        or expected["builder_source_sha256"] != sha256(ROOT / ENTRYPOINT)
    ):
        raise RuntimeError("CSV regeneration provenance changed; review it before running Docker")
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(cache, args.offline)
    verify_dictionary_resources(manifest, cache)
    archive = prepare_source(manifest, cache, args.offline)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="uqa-nori-csv-", dir=output.parent) as temporary:
        stage = Path(temporary)
        inputs = stage / "input"
        built = stage / "output"
        resources = built / "resources"
        inputs.mkdir()
        resources.mkdir(parents=True)
        extracted = extract_inputs(archive, manifest["dictionary_source"]["name"], inputs)
        actual = provenance(manifest, archive, extracted)
        if expected is not None and actual != expected:
            raise RuntimeError("CSV inputs or builder settings differ from reviewed provenance")
        command = docker_command(manifest, cache, args.platform, args.offline, ENTRYPOINT,
                                 ("/input", "/output"), output=resources, input_directory=inputs)
        completed = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True)
        fields = completed.stdout.splitlines()
        if len(fields) != len(RUNTIME_FIELDS):
            raise RuntimeError("Unexpected regeneration JVM runtime field count")
        runtime = dict(zip(RUNTIME_FIELDS, fields))
        if runtime != manifest["runtime"]:
            raise RuntimeError(f"Unexpected regeneration JVM runtime: {runtime}")
        check_resources(resources, manifest["dictionary_resources"])
        (built / "regeneration_manifest.json").write_text(canonical(actual), encoding="utf-8")
        if output.exists():
            raise RuntimeError(f"Output appeared during regeneration: {output}")
        built.rename(output)
        if args.write_manifest:
            MANIFEST_PATH.write_text(canonical(actual), encoding="utf-8")
    print(f"Verified {len(extracted)} CSV/definition inputs and {len(actual['resources'])} exact jar resources using Docker ({args.platform}): {output}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr or str(error))
        sys.exit(1)
    except (OSError, RuntimeError, ValueError, tarfile.TarError) as error:
        sys.stderr.write(f"Nori CSV regeneration failed: {error}\n")
        sys.exit(1)
