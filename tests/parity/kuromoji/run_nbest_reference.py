#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Japanese N-best cost selection, graph fixups and example grammar in Docker."""

import subprocess

from run_tokenizer_reference import ROOT, docker_command, fields, main, prepare_jars


if __name__ == "__main__":
    try:
        main(ROOT, prepare_jars, docker_command, "nbest", "KuromojiTokenizerReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
