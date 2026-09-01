#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run the complete unified-search scenario through the Python binding."""

from __future__ import annotations

import json
from typing import Optional

import uqa


GRAPH = "citations"
CURRENT_YEAR = 2026
PAPERS = [
    (
        1,
        "Learned sparse retrieval at scale",
        "sparse retrieval with learned term weights and inverted index pruning for ranking",
        "SIGIR",
        2024,
        [0.95, 0.10, 0.05],
    ),
    (
        2,
        "Block-max pruning revisited",
        "dynamic pruning for inverted index retrieval with block max bounds and ranking",
        "SIGIR",
        2019,
        [0.90, 0.15, 0.00],
    ),
    (
        3,
        "Vector quantization for dense retrieval",
        "dense retrieval with product quantization compressing embeddings for ranking",
        "NeurIPS",
        2025,
        [0.80, 0.35, 0.05],
    ),
    (
        4,
        "LSM trees under write amplification",
        "storage engines log structured merge trees and write amplification tradeoffs",
        "VLDB",
        2023,
        [0.05, 0.95, 0.10],
    ),
    (
        5,
        "Graph pattern matching in RDBMS",
        "graph pattern matching compiled into relational plans with worst case optimal joins",
        "VLDB",
        2025,
        [0.10, 0.25, 0.95],
    ),
    (
        6,
        "Regular path queries on property graphs",
        "regular path queries automata evaluation over property graphs and traversal",
        "ICDE",
        2018,
        [0.05, 0.10, 0.90],
    ),
]
ARCHIVED_PAPERS = [
    (101, "Sparse retrieval retrospective", "SIGIR", [0.93, 0.12, 0.04]),
    (102, "Dense retrieval retrospective", "NeurIPS", [0.82, 0.34, 0.06]),
    (103, "Storage engines retrospective", "VLDB", [0.08, 0.96, 0.08]),
    (104, "Graph querying retrospective", "ICDE", [0.04, 0.12, 0.94]),
]
CITES = [(3, 1), (3, 2), (1, 2), (5, 6), (5, 4)]


def recency_boost(year: Optional[int]) -> float:
    if year is None:
        return 1.0
    age = max(CURRENT_YEAR - year, 0)
    return max(0.5 ** (age / 8.0), 0.25)


def main() -> None:
    engine = uqa.Engine()
    try:
        load(engine)
        engine.register_scalar_function(
            "recency_boost",
            recency_boost,
            volatility="immutable",
            may_mutate_engine=False,
        )

        lexical = rows(
            engine,
            "SELECT id, title, year, _score FROM papers "
            "WHERE text_match(abstract, 'retrieval ranking') AND year >= 2020 "
            "ORDER BY _score DESC LIMIT 5",
        )
        bayesian = rows(
            engine,
            "SELECT id, title, year, _score FROM papers "
            "WHERE bayesian_match(abstract, 'retrieval ranking') AND year >= 2020 "
            "ORDER BY _score DESC LIMIT 5",
        )
        vector_rows = rows(
            engine,
            "SELECT id, title, venue FROM papers "
            "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 3)",
        )
        fused_exact = rows(
            engine,
            "SELECT id, title, _score FROM papers "
            "WHERE text_match(abstract, 'retrieval ranking') "
            "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4) "
            "ORDER BY _score DESC LIMIT 4",
        )
        fused_pooled = rows(
            engine,
            "SELECT id, title, _score FROM papers "
            "WHERE pool_positive_evidence("
            "bayesian_match(abstract, 'retrieval ranking'), "
            "knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4)) "
            "ORDER BY _score DESC LIMIT 4",
        )
        vector_pairs = rows(
            engine,
            "SELECT pairs.left_doc_id AS live_id, "
            "pairs.right_doc_id AS archive_id, pairs._score AS score, "
            "p.title AS live_title, a.title AS archive_title "
            "FROM vector_similarity_join("
            "papers, knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4), "
            "archived_papers, "
            "knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4), "
            "0.80) AS pairs "
            "JOIN papers AS p ON p.id = pairs.left_doc_id "
            "JOIN archived_papers AS a ON a.id = pairs.right_doc_id "
            "ORDER BY score DESC, live_id, archive_id LIMIT 8",
        )
        hybrid_pairs = rows(
            engine,
            "SELECT pairs.left_doc_id AS live_id, "
            "pairs.right_doc_id AS archive_id, pairs._score AS score, "
            "p.venue, a.title AS archive_title "
            "FROM hybrid_join("
            "papers, venue IS NOT NULL "
            "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 6), "
            "archived_papers, venue IS NOT NULL "
            "AND knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4)) AS pairs "
            "JOIN papers AS p ON p.id = pairs.left_doc_id "
            "JOIN archived_papers AS a ON a.id = pairs.right_doc_id "
            "ORDER BY score DESC, live_id, archive_id",
        )
        graph_document_pairs = rows(
            engine,
            "SELECT pairs.left_doc_id AS vertex_id, "
            "pairs.right_doc_id AS archive_id, a.title AS archive_title "
            f"FROM cross_paradigm_join(papers, graph_pagerank('{GRAPH}'), "
            "archived_papers, "
            "venue IS NOT NULL) AS pairs "
            "JOIN archived_papers AS a ON a.id = pairs.right_doc_id "
            "ORDER BY vertex_id, archive_id",
        )
        blended = rows(
            engine,
            "SELECT id, title, year, _score, "
            "_score * recency_boost(year) AS blended FROM papers "
            "WHERE text_match(abstract, 'retrieval ranking') "
            "ORDER BY blended DESC LIMIT 5",
        )
        seed = blended[0]["id"]
        cited = rows(
            engine,
            "SELECT p.id, p.title, p.venue, p.year, "
            "recency_boost(p.year) AS boost FROM papers AS p "
            f"JOIN cypher('{GRAPH}', $$ MATCH (:Paper {{paper_id: {seed}}})"
            "-[:CITES]->(cited:Paper) RETURN cited.paper_id $$) AS cited(id int) "
            "ON p.id = cited.id WHERE p.venue <> 'ICDE' ORDER BY p.year DESC",
        )
        reachable = rows(
            engine,
            "SELECT id, title, year FROM papers WHERE id IN ("
            f"SELECT id FROM cypher('{GRAPH}', $$ "
            f"MATCH (:Paper {{paper_id: {seed}}})-[:CITES]->()-[:CITES]->(r:Paper) "
            "RETURN r.paper_id $$) AS reached(id int)) ORDER BY year DESC",
        )
        unified = rows(
            engine,
            "SELECT id, title, year, _score * recency_boost(year) AS blended "
            "FROM papers WHERE text_match(abstract, 'retrieval ranking graph') "
            "AND year >= 2018 AND id IN ("
            f"SELECT id FROM cypher('{GRAPH}', $$ "
            f"MATCH (:Paper {{paper_id: {seed}}})-[:CITES*1..2]->(r:Paper) "
            "RETURN r.paper_id $$) AS reached(id int)) "
            "ORDER BY blended DESC LIMIT 5",
        )

        results = {
            "lexical": lexical,
            "bayesian": bayesian,
            "vector": vector_rows,
            "fused_exact": fused_exact,
            "fused_pooled": fused_pooled,
            "vector_pairs": vector_pairs,
            "hybrid_pairs": hybrid_pairs,
            "graph_document_pairs": graph_document_pairs,
            "blended": blended,
            "cited": cited,
            "reachable": reachable,
            "unified": unified,
        }
        verify_results(results)
        print(json.dumps(results, sort_keys=True))
    finally:
        engine.close()


def verify_results(results: dict) -> None:
    for name, result_rows in results.items():
        assert result_rows, f"unified search stage {name} returned no rows"
    assert results["blended"][0]["id"] == 3
    assert results["vector"][0]["id"] == 1
    assert all(row["live_id"] < 100 < row["archive_id"] for row in results["vector_pairs"])
    assert all(row["live_id"] < 100 < row["archive_id"] for row in results["hybrid_pairs"])
    assert all(row["archive_id"] > 100 for row in results["graph_document_pairs"])
    assert [row["id"] for row in results["cited"]] == [1, 2]
    assert [row["id"] for row in results["reachable"]] == [2]
    assert [row["id"] for row in results["unified"]] == [1, 2]


def rows(engine: object, query: str) -> list:
    return engine.sql(query).rows


def load(engine: object) -> None:
    engine.sql(
        "CREATE TABLE papers (id INTEGER PRIMARY KEY, title TEXT, abstract TEXT, "
        "venue TEXT, year INTEGER, embedding VECTOR(3))"
    )
    engine.sql("CREATE INDEX papers_text_gin ON papers USING gin (title, abstract)")
    for paper_id, title, abstract, venue, year, embedding in PAPERS:
        engine.sql(
            "INSERT INTO papers (id, title, abstract, venue, year, embedding) "
            "VALUES ($1, $2, $3, $4, $5, $6)",
            [paper_id, title, abstract, venue, year, uqa.vector(embedding)],
        )
    engine.sql("CREATE INDEX papers_embedding_hnsw ON papers USING hnsw (embedding)")
    engine.sql(
        "CREATE TABLE archived_papers (id INTEGER PRIMARY KEY, title TEXT, "
        "venue TEXT, archived_embedding VECTOR(3))"
    )
    for paper_id, title, venue, embedding in ARCHIVED_PAPERS:
        engine.sql(
            "INSERT INTO archived_papers (id, title, venue, archived_embedding) "
            "VALUES ($1, $2, $3, $4)",
            [paper_id, title, venue, uqa.vector(embedding)],
        )
    engine.sql(
        "CREATE INDEX archived_papers_embedding_hnsw "
        "ON archived_papers USING hnsw (archived_embedding)"
    )
    engine.sql(f"SELECT create_graph('{GRAPH}') AS ok")
    for paper_id, _, _, venue, _, _ in PAPERS:
        cypher(engine, f"CREATE (:Paper {{paper_id: {paper_id}, venue: '{venue}'}})")
    for source, target in CITES:
        cypher(
            engine,
            f"MATCH (a:Paper {{paper_id: {source}}}), (b:Paper {{paper_id: {target}}}) "
            "CREATE (a)-[:CITES]->(b)",
        )


def cypher(engine: object, query: str) -> None:
    engine.sql(f"SELECT * FROM cypher('{GRAPH}', $$ {query} $$) AS (ignored agtype)")


if __name__ == "__main__":
    main()
