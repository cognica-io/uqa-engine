#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Rebuild patched IPADIC in Docker and require every resource to match the pinned Kuromoji jar."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.request import urlopen

from kuromoji_runtime import ROOT, docker_command, prepare_jars, sha256, verify_dictionary_resources

sys.path.insert(0, str(ROOT.parent))
try:
    from lucene_dictionary import check_resources, extract_inputs, inventory, prepare_source
finally:
    sys.path.pop(0)

MANIFEST_PATH = ROOT / "csv_manifest.json"
ENTRYPOINT = "KuromojiDictionary.java"
RUNTIME_FIELDS = ("java_version", "java_runtime_version", "java_vendor")


def prepare_patch(manifest, cache, offline):
    patch = manifest["dictionary_patch"]
    if patch["path"] != "Noun.proper.csv.patch" or patch["target"] != "Noun.proper.csv":
        raise RuntimeError("Unexpected Japanese dictionary patch target")
    path = cache / patch["path"]
    if not path.exists():
        if offline:
            raise RuntimeError("Missing cached Japanese dictionary patch")
        with tempfile.TemporaryDirectory(prefix="kuromoji-patch-", dir=cache) as temporary:
            staged = Path(temporary) / patch["path"]
            with urlopen(patch["url"], timeout=60) as source, staged.open("wb") as output:
                shutil.copyfileobj(source, output)
            if staged.stat().st_size != patch["bytes"] or sha256(staged) != patch["sha256"]:
                raise RuntimeError("Downloaded Japanese dictionary patch checksum mismatch")
            staged.replace(path)
    if path.stat().st_size != patch["bytes"] or sha256(path) != patch["sha256"]:
        raise RuntimeError("Cached Japanese dictionary patch checksum mismatch")
    return path


def apply_patch(inputs, patch):
    # The pinned EUC-JP patch must edit exactly the existing proper-noun CSV.
    changes = subprocess.run(["git", "apply", "--numstat", str(patch)], cwd=inputs,
                             capture_output=True, text=True, check=True).stdout.splitlines()
    if len(changes) != 1 or changes[0].split("\t")[-1] != "Noun.proper.csv":
        raise RuntimeError("Japanese dictionary patch changes an unexpected file")
    subprocess.run(["git", "apply", "--check", str(patch)], cwd=inputs, check=True,
                   capture_output=True, text=True, encoding="utf-8", errors="replace")
    subprocess.run(["git", "apply", str(patch)], cwd=inputs, check=True,
                   capture_output=True, text=True, encoding="utf-8", errors="replace")


def provenance(manifest, archive, original, patched):
    return {
        "format": "uqa-kuromoji-csv-regeneration", "format_version": 1,
        "reference_manifest_sha256": sha256(ROOT / "manifest.json"),
        "builder_source_sha256": sha256(ROOT / ENTRYPOINT),
        "source_archive": {**manifest["dictionary_source"], "bytes": archive.stat().st_size},
        "patch": manifest["dictionary_patch"], "generation_recipe": manifest["generation_recipe"],
        "builder": {"class": "org.apache.lucene.analysis.ja.dict.DictionaryBuilder",
                    "format": "ipadic", "encoding": "euc-jp", "normalize_entries": False, "java_heap": "1g"},
        "runtime": manifest["runtime"], "original_inputs": original, "patched_inputs": patched,
        "resources": sorted(manifest["dictionary_resources"], key=lambda row: row["path"]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-lucene-reference-jars")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--write-manifest", action="store_true")
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        raise RuntimeError(f"Output already exists; select a new directory: {output}")
    expected = None if args.write_manifest else json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if expected is not None and (
        expected["reference_manifest_sha256"] != sha256(ROOT / "manifest.json")
        or expected["builder_source_sha256"] != sha256(ROOT / ENTRYPOINT)
    ):
        raise RuntimeError("Japanese regeneration inputs changed; review the provenance first")
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(cache, args.offline)
    verify_dictionary_resources(manifest, cache)
    archive = prepare_source(manifest, cache, args.offline)
    patch = prepare_patch(manifest, cache, args.offline)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="uqa-kuromoji-csv-", dir=output.parent) as temporary:
        stage = Path(temporary)
        inputs, built = stage / "input", stage / "output"
        resources = built / "resources"
        inputs.mkdir()
        resources.mkdir(parents=True)
        original = extract_inputs(archive, manifest["dictionary_source"]["name"], inputs)
        apply_patch(inputs, patch)
        actual = provenance(manifest, archive, original, inventory(inputs))
        if expected is not None and actual != expected:
            raise RuntimeError("Japanese source, patch or builder inputs differ from reviewed provenance")
        command = docker_command(manifest, cache, args.platform, args.offline, ENTRYPOINT,
                                 ("/input", "/output"), output=resources, input_directory=inputs)
        completed = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True, timeout=180)
        fields = completed.stdout.splitlines()
        if len(fields) != len(RUNTIME_FIELDS) or dict(zip(RUNTIME_FIELDS, fields)) != manifest["runtime"]:
            raise RuntimeError("Japanese regeneration JVM runtime differs")
        check_resources(resources, manifest["dictionary_resources"])
        text = json.dumps(actual, ensure_ascii=False, indent=2) + "\n"
        (built / "regeneration_manifest.json").write_text(text, encoding="utf-8")
        if output.exists():
            raise RuntimeError(f"Output appeared during regeneration: {output}")
        built.rename(output)
        if args.write_manifest:
            MANIFEST_PATH.write_text(text, encoding="utf-8")
    print(f"Verified {len(original)} patched IPADIC inputs and {len(actual['resources'])} exact Kuromoji resources using Docker ({args.platform})")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr or str(error))
        sys.exit(1)
    except (OSError, RuntimeError, ValueError, tarfile.TarError, subprocess.TimeoutExpired) as error:
        sys.stderr.write(f"Kuromoji CSV regeneration failed: {error}\n")
        sys.exit(1)
