#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Export and exhaustively verify the pinned Nori model using only a Docker JVM."""

import sys

import reference_runtime as runtime

sys.path.insert(0, str(runtime.ROOT.parent))
try:
    from lucene_model import ModelExport
finally:
    sys.path.pop(0)

FILES = ("lexicon.bin", "unknown.bin", "connection_costs.bin", "characters.bin", "unicode.bin")
EXPORTER = ModelExport(
    runtime, "Nori", "NoriModel.java", FILES,
    ("lucene_version", "lucene_commit", "docker_image", "runtime", "jars",
     "dictionary_source", "dictionary_resources"),
    "uqa-nori-reference-jars",
)


if __name__ == "__main__":
    sys.exit(EXPORTER.run_cli())
