#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify user-dictionary compilation with the pinned Docker-only JVM."""

import subprocess

from reference_cases import encoded, main


if __name__ == "__main__":
    try:
        main("user", "NoriUserReference.java", lambda case: [encoded(case[key]) for key in ["rules", "query"]], __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
