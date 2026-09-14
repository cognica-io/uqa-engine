#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Export and exhaustively verify the pinned Nori model using only a Docker JVM."""

import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

from reference_runtime import ROOT, docker_command, prepare_jars, sha256, verify_dictionary_resources


FILES = ("lexicon.bin", "unknown.bin", "connection_costs.bin", "characters.bin", "unicode.bin")
MANIFEST_PATH = ROOT / "model_manifest.json"


def manifest_text(value):
    return json.dumps(value, ensure_ascii=False, indent=2) + "\n"


def file_inventory(directory):
    return [
        {"path": name, "bytes": (directory / name).stat().st_size, "sha256": sha256(directory / name)}
        for name in FILES
    ]


def check_files(directory, expected):
    names = {item["path"] for item in expected["files"]}
    if names != set(FILES) or len(expected["files"]) != len(FILES):
        raise RuntimeError("Unexpected neutral-model file inventory")
    if {path.name for path in directory.iterdir()} != names | {"model_manifest.json"}:
        raise RuntimeError("Unexpected files in exported model directory")
    for item in expected["files"]:
        path = directory / item["path"]
        if path.stat().st_size != item["bytes"] or sha256(path) != item["sha256"]:
            raise RuntimeError(f"Exported model checksum mismatch: {path}")
    stored = json.loads((directory / "model_manifest.json").read_text(encoding="utf-8"))
    if stored != expected:
        raise RuntimeError("Stored model manifest differs from the pinned export")


def run_model(manifest, cache, args, directory, verify):
    command = docker_command(
        manifest, cache, args.platform, args.offline, "NoriModel.java",
        ("verify" if verify else "export", "/output"), output=directory, output_readonly=verify,
    )
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True)
    model = json.loads(result.stdout)
    if model["runtime"] != manifest["runtime"]:
        raise RuntimeError(f"Unexpected model JVM runtime: {model['runtime']}")
    return model


def summarize(manifest, model, directory):
    return {
        "format": "uqa-nori-neutral",
        "format_version": 1,
        "byte_order": "big",
        "exporter_sha256": sha256(ROOT / "NoriModel.java"),
        "reference": {
            name: manifest[name]
            for name in ("lucene_version", "lucene_commit", "docker_image", "runtime",
                         "jars", "dictionary_source", "dictionary_resources")
        },
        "model": model,
        "files": file_inventory(directory),
    }


def compare_manifest(actual, expected):
    if actual != expected:
        difference = "".join(difflib.unified_diff(
            manifest_text(expected).splitlines(keepends=True),
            manifest_text(actual).splitlines(keepends=True),
            fromfile="pinned model manifest", tofile="Docker model export",
        ))
        raise RuntimeError("Model export differs from the reviewed manifest:\n" + difference)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New output directory, or existing export with --verify-only")
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-nori-reference-jars")
    parser.add_argument("--offline", action="store_true", help="Require cached jars and Docker image")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    parser.add_argument("--verify-only", action="store_true", help="Check hashes and compare every field of an existing export with Lucene")
    parser.add_argument("--write-manifest", action="store_true", help="Record an intentional, reviewed exporter or reference change")
    args = parser.parse_args()
    if args.verify_only and args.write_manifest:
        parser.error("--verify-only cannot change the pinned manifest")
    output = args.output.resolve()
    if not args.verify_only and output.exists():
        raise RuntimeError(f"Output already exists; use --verify-only or a new directory: {output}")
    expected = None if args.write_manifest else json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if args.verify_only:
        check_files(output, expected)
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(cache, args.offline)
    verify_dictionary_resources(manifest, cache)
    if args.verify_only:
        model = run_model(manifest, cache, args, output, True)
        compare_manifest(summarize(manifest, model, output), expected)
    else:
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="uqa-nori-export-", dir=output.parent) as temporary:
            staging = Path(temporary)
            model = run_model(manifest, cache, args, staging, False)
            actual = summarize(manifest, model, staging)
            if expected is not None:
                compare_manifest(actual, expected)
            (staging / "model_manifest.json").write_text(manifest_text(actual), encoding="utf-8")
            if output.exists():
                raise RuntimeError(f"Output appeared during export: {output}")
            staging.rename(output)
            if args.write_manifest:
                MANIFEST_PATH.write_text(manifest_text(actual), encoding="utf-8")
    print(
        f"Verified {model['surface_count']} surfaces, {model['word_count']} system words, "
        f"{model['unknown_word_count']} unknown words, "
        f"{model['matrix_forward'] * model['matrix_backward']} connection costs, "
        f"and {model['unicode_count']} Unicode values using Docker ({args.platform}): {output}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr or str(error))
        sys.exit(1)
    except (OSError, RuntimeError, ValueError) as error:
        sys.stderr.write(f"Nori model export failed: {error}\n")
        sys.exit(1)
