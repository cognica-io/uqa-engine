#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Print the browser corpus contract through an installed native Python binding."""

import hashlib
import json
from pathlib import Path

import uqa


def main():
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
    print(json.dumps({"schema_version": 1, "corpus_sha256": hashlib.sha256(corpus_path.read_bytes()).hexdigest(), "checks": checks},
                     ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
