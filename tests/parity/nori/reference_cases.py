#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Pinned, lossless fixture transport for standalone Docker reference drivers."""

import subprocess
import sys

from reference_runtime import ROOT, docker_command, prepare_jars

sys.path.insert(0, str(ROOT.parent))
try:
    import lucene_cases
    from lucene_cases import canonical_output, encoded, inventory
finally:
    sys.path.pop(0)


def provenance(stem, entrypoint, cases, output):
    return lucene_cases.provenance(ROOT, stem, entrypoint, cases, output)


def main(stem, entrypoint, fields, description):
    return lucene_cases.main(ROOT, prepare_jars, docker_command, stem, entrypoint, fields, description,
                             cache_name="uqa-nori-reference-jars")
