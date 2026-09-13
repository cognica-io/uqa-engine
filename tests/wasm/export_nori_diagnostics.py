#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Print and optionally verify the corpus contract through an installed native Python binding."""

import argparse
import difflib
import hashlib
import json
from pathlib import Path
import sys

import uqa


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", type=Path, help="compare with this reviewed contract and print any difference")
    args = parser.parse_args()
    corpus_path = Path(__file__).resolve().parents[2] / "crates/uqa-analysis/benches/nori/corpus.json"
    corpus = json.loads(corpus_path.read_text(encoding="utf-8"))
    engine = uqa.Engine()
    checks = []
    try:
        for mode in ["none", "discard", "mixed"]:
            name = "browser_nori_" + mode
            config = {"tokenizer": {"type": "nori_tokenizer", "decompound_mode": mode},
                      "token_filters": [{"type": "nori_part_of_speech"}, {"type": "nori_readingform"}, {"type": "unicode_simple_lowercase"}]}
            engine.sql("SELECT * FROM create_analyzer($1, $2)", [name, json.dumps(config)])
            for case in corpus["cases"]:
                text = case["text"] * case["repeat"]
                analysis = engine.sql("SELECT analysis FROM analyze_text($1, $2)", [name, text]).rows[0]["analysis"]
                encoded = json.dumps(analysis, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
                checks.append({"mode": mode, "name": case["name"], "input_utf8_bytes": len(text.encode()),
                               "token_count": len(analysis["tokens"]), "analyzer_fingerprint": analysis["analyzer_fingerprint"],
                               "diagnostic_sha256": hashlib.sha256(encoded).hexdigest()})
    finally:
        engine.close()
    actual = {"schema_version": 1, "corpus_sha256": hashlib.sha256(corpus_path.read_bytes()).hexdigest(), "checks": checks}
    print(json.dumps(actual, ensure_ascii=False, indent=2))
    if args.check:
        expected = json.loads(args.check.read_text(encoding="utf-8"))
        if actual != expected:
            before = json.dumps(expected, sort_keys=True, indent=2).splitlines(keepends=True)
            after = json.dumps(actual, sort_keys=True, indent=2).splitlines(keepends=True)
            sys.stderr.writelines(difflib.unified_diff(before, after, fromfile="expected", tofile="native"))
            raise SystemExit("Native Nori diagnostic identity differs from the reviewed corpus")


if __name__ == "__main__":
    main()
