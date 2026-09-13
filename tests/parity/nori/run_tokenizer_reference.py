#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify complete tokenizer output with the pinned Docker-only JVM."""

import subprocess

from reference_cases import encoded, main


def fields(case):
    return [
        case["decompound_mode"].upper(),
        str(case["output_unknown_unigrams"]).lower(),
        str(case["discard_punctuation"]).lower(),
        encoded(case["input"]),
        "-" if case["user_dictionary"] is None else encoded(case["user_dictionary"]),
    ]


if __name__ == "__main__":
    try:
        main("tokenizer", "NoriTokenizerReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
