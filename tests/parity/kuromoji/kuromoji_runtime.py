#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Japanese reference resources using the shared pinned Lucene Docker runner."""

import hashlib
from pathlib import Path
import sys
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent))
try:
    import lucene_runtime
finally:
    sys.path.pop(0)

sha256 = lucene_runtime.sha256


def prepare_jars(cache, offline):
    return lucene_runtime.prepare_jars(ROOT, cache, offline)


def docker_command(manifest, cache, platform, offline, entrypoint, arguments=(), output=None, output_readonly=False, input_directory=None):
    return lucene_runtime.docker_command(
        ROOT, manifest, cache, platform, offline, entrypoint, arguments,
        output, output_readonly, input_directory,
    )


def verify_dictionary_resources(manifest, cache):
    path = cache / ("lucene-analysis-kuromoji-" + manifest["lucene_version"] + ".jar")
    expected = {item["path"]: item for item in manifest["dictionary_resources"] + manifest["analysis_resources"]}
    with ZipFile(path) as archive:
        actual = [name for name in archive.namelist()
                  if name.startswith("org/apache/lucene/analysis/ja/") and not name.endswith(("/", ".class"))]
        if sorted(actual) != sorted(expected):
            raise RuntimeError("Japanese resource inventory differs from the pinned manifest")
        for name, item in expected.items():
            data = archive.read(name)
            if len(data) != item["bytes"] or hashlib.sha256(data).hexdigest() != item["sha256"]:
                raise RuntimeError(f"Japanese resource checksum mismatch: {name}")
