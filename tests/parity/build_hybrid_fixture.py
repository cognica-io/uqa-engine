#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Captures hybrid (text + KNN, log-odds fused) UQA outputs.

The UQA-RS implementation's integration test
(`crates/uqa-engine/tests/hybrid_search_parity.rs`) loads this fixture and
asserts identical doc id ordering and matching scores within a small
epsilon.

Refresh the fixture:

    python3 tests/parity/build_hybrid_fixture.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parents[2]
PY_UQA = REPO.parent / "uqa"
sys.path.insert(0, str(PY_UQA))

from bayesian_bm25 import (  # noqa: E402
    cosine_to_probability,
    log_odds_conjunction,
)

from uqa.analysis.analyzer import standard_analyzer  # noqa: E402
from uqa.core.posting_list import PostingList  # noqa: E402
from uqa.core.types import Payload, PostingEntry  # noqa: E402
from uqa.operators.base import ExecutionContext  # noqa: E402
from uqa.operators.primitive import KNNOperator, ScoreOperator, TermOperator  # noqa: E402
from uqa.scoring.bayesian_bm25 import BayesianBM25Params, BayesianBM25Scorer  # noqa: E402
from uqa.storage.document_store import MemoryDocumentStore  # noqa: E402
from uqa.storage.inverted_index import MemoryInvertedIndex  # noqa: E402


def _local_vector_index(dimensions: int):
    """Tiny in-memory vector index for deterministic parity tests.

    The upstream MemoryVectorIndex has been replaced with IVF in
    production; we mirror the brute-force semantics that the Rust
    `MemoryVectorIndex` uses.
    """

    class _MemoryVectorIndex:
        def __init__(self, dim: int) -> None:
            self.dimensions = dim
            self._vectors: dict[int, np.ndarray] = {}

        def add(self, doc_id: int, vector: np.ndarray) -> None:
            self._vectors[doc_id] = np.asarray(vector, dtype=np.float32)

        def delete(self, doc_id: int) -> None:
            self._vectors.pop(doc_id, None)

        def clear(self) -> None:
            self._vectors.clear()

        def search_knn(self, query: np.ndarray, k: int) -> PostingList:
            q = np.asarray(query, dtype=np.float32)
            qn = float(np.linalg.norm(q))
            if qn == 0:
                return PostingList()
            scored: list[tuple[int, float]] = []
            for doc_id, v in self._vectors.items():
                vn = float(np.linalg.norm(v))
                if vn == 0:
                    continue
                sim = float(np.dot(q, v) / (qn * vn))
                scored.append((doc_id, sim))
            scored.sort(key=lambda x: (-x[1], x[0]))
            scored = scored[:k]
            scored.sort(key=lambda x: x[0])
            entries = [PostingEntry(d, Payload(score=s)) for d, s in scored]
            return PostingList.from_sorted(entries)

        def search_threshold(
            self, query: np.ndarray, threshold: float
        ) -> PostingList:
            q = np.asarray(query, dtype=np.float32)
            qn = float(np.linalg.norm(q))
            if qn == 0:
                return PostingList()
            entries: list[PostingEntry] = []
            for doc_id, v in sorted(self._vectors.items()):
                vn = float(np.linalg.norm(v))
                if vn == 0:
                    continue
                sim = float(np.dot(q, v) / (qn * vn))
                if sim >= threshold:
                    entries.append(PostingEntry(doc_id, Payload(score=sim)))
            return PostingList.from_sorted(entries)

        def count(self) -> int:
            return len(self._vectors)

    return _MemoryVectorIndex(dimensions)


CORPUS = [
    (1, "the rust programming language", [1.0, 0.1, 0.0]),
    (2, "rust ecosystem and crates", [0.9, 0.0, 0.1]),
    (3, "python programming guide", [0.0, 1.0, 0.0]),
    (4, "java enterprise patterns", [0.0, 0.0, 1.0]),
    (5, "rust idioms and ownership", [0.95, 0.05, 0.05]),
    (6, "go concurrency primitives", [0.1, 0.9, 0.0]),
    (7, "rust standard library", [0.85, 0.1, 0.15]),
    (8, "memory safety zero overhead", [0.7, 0.3, 0.1]),
    (9, "garbage collection tradeoffs", [0.05, 0.5, 0.5]),
    (10, "no nonsense systems language", [0.5, 0.5, 0.5]),
]

VECTOR_DIM = 3


def _coverage_default(n_hits: int, n_total: int, floor: float = 0.01) -> float:
    if n_total == 0:
        return 0.5
    r = n_hits / n_total
    return 0.5 * (1.0 - r) + floor * r


def run_hybrid(
    docs: MemoryDocumentStore,
    idx: MemoryInvertedIndex,
    vidx,
    text_field: str,
    text_query: str,
    query_vector: list[float],
    knn_pool: int,
    alpha: float,
    top_k: int,
) -> list[dict[str, float]]:
    ctx = ExecutionContext(
        document_store=docs,
        inverted_index=idx,
        vector_indexes={"embedding": vidx},
    )

    # Text signal: Bayesian BM25 over text_field.
    term_op = TermOperator(text_query, text_field)
    bayes = BayesianBM25Scorer(BayesianBM25Params(), idx.stats)
    analyzer = idx.get_search_analyzer(text_field)
    terms = analyzer.analyze(text_query)
    text_pl = ScoreOperator(bayes, term_op, terms, field=text_field).execute(ctx)
    text_map = {e.doc_id: float(e.payload.score) for e in text_pl}

    # Vector signal: KNN cosine similarity, then cosine_to_probability.
    knn_op = KNNOperator(np.asarray(query_vector, dtype=np.float32), knn_pool, field="embedding")
    knn_pl = knn_op.execute(ctx)
    vector_map = {
        e.doc_id: float(cosine_to_probability(e.payload.score)) for e in knn_pl
    }

    all_ids = sorted(set(text_map) | set(vector_map))
    n = len(all_ids)
    text_default = _coverage_default(len(text_map), n)
    vector_default = _coverage_default(len(vector_map), n)

    results: list[dict[str, float]] = []
    for doc_id in all_ids:
        p_text = text_map.get(doc_id, text_default)
        p_vector = vector_map.get(doc_id, vector_default)
        fused = float(
            log_odds_conjunction(np.array([p_text, p_vector]), alpha=alpha)
        )
        results.append({"doc_id": doc_id, "score": fused})

    results.sort(key=lambda r: (-r["score"], r["doc_id"]))
    return results[:top_k]


def main() -> None:
    analyzer = standard_analyzer()
    docs = MemoryDocumentStore()
    idx = MemoryInvertedIndex(analyzer=analyzer)
    vidx = _local_vector_index(VECTOR_DIM)
    for doc_id, title, vec in CORPUS:
        docs.put(doc_id, {"title": title})
        idx.add_document(doc_id, {"title": title})
        vidx.add(doc_id, np.asarray(vec, dtype=np.float32))

    queries = [
        {
            "text_field": "title",
            "text_query": "rust",
            "query_vector": [1.0, 0.0, 0.0],
            "knn_pool": 10,
            "alpha": 0.5,
            "top_k": 10,
        },
        {
            "text_field": "title",
            "text_query": "language",
            "query_vector": [1.0, 0.1, 0.0],
            "knn_pool": 5,
            "alpha": 0.5,
            "top_k": 5,
        },
        {
            "text_field": "title",
            "text_query": "rust",
            "query_vector": [0.0, 1.0, 0.0],
            "knn_pool": 10,
            "alpha": 0.0,  # plain mean log-odds, scale-neutral
            "top_k": 10,
        },
    ]

    cases = []
    for q in queries:
        expected = run_hybrid(
            docs,
            idx,
            vidx,
            q["text_field"],
            q["text_query"],
            q["query_vector"],
            q["knn_pool"],
            q["alpha"],
            q["top_k"],
        )
        cases.append({**q, "expected": expected})

    fixture = {
        "version": 1,
        "vector_dim": VECTOR_DIM,
        "corpus": [
            {"id": doc_id, "title": title, "embedding": vec}
            for (doc_id, title, vec) in CORPUS
        ],
        "queries": cases,
    }

    out = REPO / "tests" / "parity" / "hybrid_search_fixture.json"
    out.write_text(json.dumps(fixture, indent=2) + "\n")
    print(f"wrote {out} ({len(cases)} hybrid queries)")


if __name__ == "__main__":
    main()
