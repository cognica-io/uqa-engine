#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Japanese user-dictionary CSV and lookup with the pinned Docker-only JVM."""

import subprocess
import sys

from kuromoji_runtime import ROOT, docker_command, prepare_jars

sys.path.insert(0, str(ROOT.parent))
try:
    from lucene_cases import encoded, main
finally:
    sys.path.pop(0)


if __name__ == "__main__":
    try:
        main(ROOT, prepare_jars, docker_command, "user", "KuromojiUserReference.java",
             lambda case: [encoded(case[key]) for key in ["rules", "query"]], __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
