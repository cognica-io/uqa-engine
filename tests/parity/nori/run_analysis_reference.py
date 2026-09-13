#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Korean filters, complete analysis, and normalization with Docker-only Lucene."""

import subprocess

from reference_cases import main
from run_tokenizer_reference import fields as tokenizer_fields


def fields(case):
    filters = []
    for item in case["filters"]:
        kind = item["type"]
        if kind == "nori_part_of_speech":
            tags = item.get("stop_tags")
            filters.append("pos:" + ("*" if tags is None else ",".join(tags)))
        elif kind == "nori_readingform":
            filters.append("reading")
        elif kind == "unicode_simple_lowercase":
            filters.append("lowercase")
        else:
            raise ValueError(f"Unknown filter: {kind}")
    return [*tokenizer_fields(case), case["pipeline"], ";".join(filters)]


if __name__ == "__main__":
    try:
        main("analysis", "NoriAnalysisReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
