#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Atomic binary-model export and read-only verification through a pinned Docker oracle."""

import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

from lucene_runtime import sha256


def manifest_text(value):
    return json.dumps(value, ensure_ascii=False, indent=2) + "\n"


def compare_manifest(actual, expected):
    if actual != expected:
        difference = "".join(difflib.unified_diff(
            manifest_text(expected).splitlines(keepends=True),
            manifest_text(actual).splitlines(keepends=True),
            fromfile="pinned model manifest", tofile="Docker model export",
        ))
        raise RuntimeError("Model export differs from the reviewed manifest:\n" + difference)


class ModelExport:
    def __init__(self, runtime, name, entrypoint, files, reference_fields, cache_name):
        self.runtime = runtime
        self.name = name
        self.entrypoint = entrypoint
        self.files = files
        self.reference_fields = reference_fields
        self.cache_name = cache_name

    def file_inventory(self, directory):
        return [
            {"path": name, "bytes": (directory / name).stat().st_size, "sha256": sha256(directory / name)}
            for name in self.files
        ]

    def check_files(self, directory, expected):
        names = {item["path"] for item in expected["files"]}
        if names != set(self.files) or len(expected["files"]) != len(self.files):
            raise RuntimeError("Unexpected neutral-model file inventory")
        if {path.name for path in directory.iterdir()} != names | {"model_manifest.json"}:
            raise RuntimeError("Unexpected files in exported model directory")
        for name in names | {"model_manifest.json"}:
            path = directory / name
            if path.is_symlink() or not path.is_file():
                raise RuntimeError(f"Exported model entry is not a regular file: {name}")
        for item in expected["files"]:
            path = directory / item["path"]
            if path.stat().st_size != item["bytes"] or sha256(path) != item["sha256"]:
                raise RuntimeError(f"Exported model checksum mismatch: {path}")
        stored = json.loads((directory / "model_manifest.json").read_text(encoding="utf-8"))
        if stored != expected:
            raise RuntimeError("Stored model manifest differs from the pinned export")

    def run_model(self, manifest, cache, args, directory, verify):
        command = self.runtime.docker_command(
            manifest, cache, args.platform, args.offline, self.entrypoint,
            ("verify" if verify else "export", "/output"), output=directory, output_readonly=verify,
        )
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True, timeout=300)
        model = json.loads(result.stdout)
        if model["runtime"] != manifest["runtime"]:
            raise RuntimeError(f"Unexpected model JVM runtime: {model['runtime']}")
        return model

    def summarize(self, manifest, model, directory):
        return {
            "format": f"uqa-{self.name.lower()}-neutral",
            "format_version": 1,
            "byte_order": "big",
            "exporter_sha256": sha256(self.runtime.ROOT / self.entrypoint),
            "reference": {name: manifest[name] for name in self.reference_fields},
            "model": model,
            "files": self.file_inventory(directory),
        }

    def main(self):
        parser = argparse.ArgumentParser(description=f"Export and verify the complete pinned {self.name} model using Docker.")
        parser.add_argument("--output", type=Path, required=True, help="New directory, or existing export with --verify-only")
        parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / self.cache_name)
        parser.add_argument("--offline", action="store_true", help="Require cached jars and Docker image")
        parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
        parser.add_argument("--verify-only", action="store_true", help="Check hashes and every field against the read-only oracle")
        parser.add_argument("--write-manifest", action="store_true", help="Record a reviewed exporter or reference change")
        args = parser.parse_args()
        if args.verify_only and args.write_manifest:
            parser.error("--verify-only cannot change the pinned manifest")
        output = args.output.resolve()
        if not args.verify_only and output.exists():
            raise RuntimeError(f"Output already exists; use --verify-only or a new directory: {output}")
        manifest_path = self.runtime.ROOT / "model_manifest.json"
        expected = None if args.write_manifest else json.loads(manifest_path.read_text(encoding="utf-8"))
        if args.verify_only:
            self.check_files(output, expected)
        cache = args.cache_dir.resolve()
        manifest = self.runtime.prepare_jars(cache, args.offline)
        self.runtime.verify_dictionary_resources(manifest, cache)
        if args.verify_only:
            model = self.run_model(manifest, cache, args, output, True)
            compare_manifest(self.summarize(manifest, model, output), expected)
        else:
            output.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix=f"uqa-{self.name.lower()}-export-", dir=output.parent) as temporary:
                staging = Path(temporary)
                model = self.run_model(manifest, cache, args, staging, False)
                actual = self.summarize(manifest, model, staging)
                if expected is not None:
                    compare_manifest(actual, expected)
                (staging / "model_manifest.json").write_text(manifest_text(actual), encoding="utf-8")
                self.check_files(staging, actual)
                if output.exists():
                    raise RuntimeError(f"Output appeared during export: {output}")
                staging.rename(output)
                if args.write_manifest:
                    manifest_path.write_text(manifest_text(actual), encoding="utf-8")
        print(
            f"Verified {model['surface_count']} surfaces, {model['word_count']} system words, "
            f"{model['unknown_word_count']} unknown words, "
            f"{model['matrix_forward'] * model['matrix_backward']} connection costs, "
            f"and {model['unicode_count']} Unicode values using Docker ({args.platform}): {output}"
        )
        return 0

    def run_cli(self):
        try:
            return self.main()
        except subprocess.CalledProcessError as error:
            sys.stderr.write(error.stderr or str(error))
        except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
            sys.stderr.write(f"{self.name} model export failed: {error}\n")
        return 1
