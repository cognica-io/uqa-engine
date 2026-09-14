#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Japanese tokenizer graphs and all six attributes with the pinned Docker-only JVM."""

import base64
import subprocess
import sys

from kuromoji_runtime import ROOT, docker_command, prepare_jars

sys.path.insert(0, str(ROOT.parent))
try:
    from lucene_cases import encoded, main
finally:
    sys.path.pop(0)


def fields(case):
    if "input_utf16" in case:
        raw = b"".join(unit.to_bytes(2, "big") for unit in case["input_utf16"])
    else:
        text = case["input"] * case.get("repeat", 1)
        raw = text.encode("utf-16-be")
    return [
        case["mode"].upper(),
        str(case["discard_punctuation"]).lower(),
        str(case["discard_compound_token"]).lower(),
        base64.b64encode(raw).decode("ascii"),
        "-" if case.get("user_dictionary") is None else encoded(case["user_dictionary"]),
        str(case.get("n_best_cost", 0)),
        "-" if case.get("n_best_examples") is None else encoded(case["n_best_examples"]),
    ]


if __name__ == "__main__":
    try:
        main(ROOT, prepare_jars, docker_command, "tokenizer", "KuromojiTokenizerReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
