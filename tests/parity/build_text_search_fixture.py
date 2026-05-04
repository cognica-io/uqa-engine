#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Captures Python UQA text-search outputs into a JSON fixture.

The Rust port's integration test (`crates/uqa-engine/tests/text_search_parity.rs`)
loads this fixture and asserts identical doc id ordering plus matching
scores within a small epsilon. Run once to refresh the fixture:

    python3 tests/parity/build_text_search_fixture.py

The fixture is committed so the Rust test runs without a Python toolchain.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PY_UQA = REPO.parent / "uqa"
sys.path.insert(0, str(PY_UQA))

from uqa.analysis.analyzer import standard_analyzer  # noqa: E402
from uqa.core.posting_list import PostingList  # noqa: E402
from uqa.core.types import IndexStats, Payload, PostingEntry  # noqa: E402
from uqa.operators.base import ExecutionContext  # noqa: E402
from uqa.operators.primitive import ScoreOperator, TermOperator  # noqa: E402
from uqa.scoring.bayesian_bm25 import BayesianBM25Params, BayesianBM25Scorer  # noqa: E402
from uqa.scoring.bm25 import BM25Params, BM25Scorer  # noqa: E402
from uqa.storage.document_store import MemoryDocumentStore  # noqa: E402
from uqa.storage.inverted_index import MemoryInvertedIndex  # noqa: E402


CORPUS = [
    (1, "the rust programming language", "Rust is a systems language."),
    (2, "python programming guide", "Dynamic typing and garbage collection."),
    (3, "rust rust rust everywhere", "How rust ate the systems world."),
    (4, "the c programming language", "Classic systems language."),
    (5, "go in action", "Concurrency primitives in the Go language."),
    (6, "advanced rust idioms", "Lifetimes, ownership, and borrowing in rust."),
    (7, "fearless concurrency", "Rust's ownership model prevents data races."),
    (8, "java enterprise patterns", "Spring framework and the JVM."),
    (9, "the rust standard library", "Iterators, collections, and traits in rust."),
    (10, "memory safety without gc", "How rust achieves safety with zero overhead."),
]


def build_index() -> tuple[MemoryDocumentStore, MemoryInvertedIndex]:
    analyzer = standard_analyzer()
    docs = MemoryDocumentStore()
    idx = MemoryInvertedIndex(analyzer=analyzer)
    for doc_id, title, body in CORPUS:
        docs.put(doc_id, {"title": title, "body": body})
        idx.add_document(doc_id, {"title": title, "body": body})
    return docs, idx


def run_query(
    docs: MemoryDocumentStore,
    idx: MemoryInvertedIndex,
    field: str,
    query: str,
    scoring: str,
    top_k: int,
) -> list[dict[str, float]]:
    ctx = ExecutionContext(document_store=docs, inverted_index=idx)
    term_op = TermOperator(query, field)

    if scoring == "bm25":
        scorer = BM25Scorer(BM25Params(), idx.stats)
    elif scoring == "bayesian_bm25":
        scorer = BayesianBM25Scorer(BayesianBM25Params(), idx.stats)
    else:
        raise ValueError(f"unknown scoring: {scoring}")

    analyzer = idx.get_search_analyzer(field)
    terms = analyzer.analyze(query)

    score_op = ScoreOperator(scorer, term_op, terms, field=field)
    pl = score_op.execute(ctx)
    entries = list(pl)

    entries.sort(key=lambda e: (-e.payload.score, e.doc_id))
    entries = entries[:top_k]
    return [{"doc_id": e.doc_id, "score": float(e.payload.score)} for e in entries]


def main() -> None:
    docs, idx = build_index()

    queries = [
        ("title", "rust", "bm25", 10),
        ("title", "language", "bm25", 10),
        ("title", "rust language", "bm25", 10),
        ("title", "rust", "bayesian_bm25", 10),
        ("title", "language", "bayesian_bm25", 10),
        ("title", "rust language", "bayesian_bm25", 10),
        ("body", "rust", "bm25", 10),
        ("body", "ownership", "bm25", 10),
    ]

    cases = []
    for field, query, scoring, top_k in queries:
        expected = run_query(docs, idx, field, query, scoring, top_k)
        cases.append({
            "field": field,
            "query": query,
            "scoring": scoring,
            "top_k": top_k,
            "expected": expected,
        })

    fixture = {
        "version": 1,
        "corpus": [
            {"id": doc_id, "title": title, "body": body}
            for (doc_id, title, body) in CORPUS
        ],
        "queries": cases,
    }

    out = REPO / "tests" / "parity" / "text_search_fixture.json"
    out.write_text(json.dumps(fixture, indent=2) + "\n")
    print(f"wrote {out} ({len(cases)} queries)")


if __name__ == "__main__":
    main()
