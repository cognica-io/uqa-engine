#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify exact Korean numbers and shared token attributes with Docker-only Lucene."""

import base64
import subprocess

from reference_cases import encoded, main


def hexadecimal(units):
    return "".join(f"{unit:04x}" for unit in units)


def input_units(case):
    if "input_utf16" in case:
        return case["input_utf16"]
    data = case.get("input", "").encode("utf-16-be")
    return [int.from_bytes(data[index:index + 2], "big") for index in range(0, len(data), 2)]


def token_row(token):
    reading = token.get("reading_utf16")
    parts = token.get("morphemes")
    return "\t".join([
        hexadecimal(token["term_utf16"]), str(token["start_utf16"]), str(token["end_utf16"]),
        str(token["position_increment"]), str(token["position_length"]), str(token["keyword"]).lower(),
        token.get("pos_type") or "-", token.get("left_pos") or "-", token.get("right_pos") or "-",
        "-" if reading is None else hexadecimal(reading),
        "-" if parts is None else ";".join(part["pos"] + ":" + hexadecimal(part["surface_utf16"]) for part in parts),
    ])


def fields(case):
    filters = []
    for item in case.get("filters", []):
        kind = item["type"]
        if kind == "nori_part_of_speech":
            tags = item.get("stop_tags")
            filters.append("pos:" + ("*" if tags is None else ",".join(tags)))
        else:
            filters.append({"nori_number": "number", "nori_readingform": "reading", "unicode_simple_lowercase": "lowercase"}[kind])
    tokens = "\n".join(token_row(token) for token in case.get("tokens", []))
    rules = case.get("user_dictionary")
    return [
        case["pipeline"], hexadecimal(input_units(case)), case.get("decompound_mode", "none").upper(),
        str(case.get("output_unknown_unigrams", False)).lower(), str(case.get("discard_punctuation", False)).lower(),
        "-" if rules is None else encoded(rules), ";".join(filters), base64.b64encode(tokens.encode("ascii")).decode("ascii"),
        str(case.get("final_offset_utf16", 0)), str(case.get("final_position_increment", 0)),
    ]


if __name__ == "__main__":
    try:
        main("number", "NoriNumberReference.java", fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
