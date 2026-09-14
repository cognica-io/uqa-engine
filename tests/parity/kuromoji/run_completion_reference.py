#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Japanese ordered completion alternatives using the pinned Docker JVM."""

import subprocess

from run_filter_reference import ROOT, docker_command, fields, main, prepare_jars


if __name__ == '__main__':
    try:
        main(ROOT, prepare_jars, docker_command, 'completion', 'KuromojiFilterReference.java', fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
