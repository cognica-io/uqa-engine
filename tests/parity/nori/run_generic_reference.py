#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Korean filters over generic token streams with the Docker-only reference."""

import subprocess

from reference_cases import main
from run_number_reference import fields


if __name__ == "__main__":
    try:
        main("generic", "NoriNumberReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
