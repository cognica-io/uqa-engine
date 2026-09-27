#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Compare vector indexes and reopen DiskANN after committed and rolled-back writes."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import uqa


CORPUS = [
    (1, "async runtimes", "systems", [0.95, 0.10, 0.05, 0.00]),
    (2, "ownership and borrows", "systems", [0.90, 0.20, 0.00, 0.10]),
    (3, "zero-copy parsing", "systems", [0.85, 0.05, 0.15, 0.05]),
    (4, "sourdough starters", "cooking", [0.05, 0.95, 0.10, 0.00]),
    (5, "knife skills", "cooking", [0.00, 0.90, 0.20, 0.05]),
    (6, "fermentation basics", "cooking", [0.10, 0.85, 0.05, 0.15]),
]


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="uqa-python-vector-") as directory:
        path = Path(directory) / "vectors.db"
        engine = uqa.open(path)
        try:
            results = scenario(engine)
        finally:
            engine.close()
        reopened = uqa.open(path)
        try:
            results["reopened"] = knn(reopened, 6)
            assert results["reopened"] == results["after_commit"]
            method = reopened.sql(
                "SELECT indexdef FROM pg_indexes WHERE indexname = 'notes_embedding_diskann'"
            ).rows[0]["indexdef"]
            assert "using diskann" in method.lower()
        finally:
            reopened.close()
        print(json.dumps(results, sort_keys=True))


def scenario(engine: object) -> dict:
    engine.sql(
        "CREATE TABLE notes ("
        "id INTEGER PRIMARY KEY, title TEXT, topic TEXT, embedding VECTOR(4))"
    )
    for doc_id, title, topic, embedding in CORPUS:
        engine.sql(
            "INSERT INTO notes (id, title, topic, embedding) VALUES ($1, $2, $3, $4)",
            [doc_id, title, topic, uqa.vector(embedding)],
        )

    results = {"exact": knn(engine)}
    engine.sql("CREATE INDEX notes_embedding_hnsw ON notes USING hnsw (embedding)")
    results["hnsw"] = knn(engine)
    engine.sql("DROP INDEX notes_embedding_hnsw")
    engine.sql(
        "CREATE INDEX notes_embedding_ivf ON notes USING ivf (embedding) "
        "WITH (lists = 2, probes = 2, train_threshold = 4)"
    )
    results["ivf"] = knn(engine)
    engine.sql("DROP INDEX notes_embedding_ivf")
    engine.sql(
        "CREATE INDEX notes_embedding_diskann ON notes USING diskann (embedding) "
        "WITH (max_degree = 4, search_list_size = 16, beam_width = 2)"
    )
    results["diskann"] = knn(engine)
    assert results["diskann"] == results["exact"]
    assert [row["id"] for row in results["diskann"]] == [1, 3, 2]
    results["filtered"] = engine.sql(
        "SELECT id, title, topic FROM notes "
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) "
        "AND topic = 'cooking' ORDER BY _score DESC, id LIMIT 3"
    ).rows

    assert results["exact"][0]["id"] == 1
    assert results["hnsw"][0]["id"] == 1
    assert results["ivf"][0]["id"] == 1
    assert [row["topic"] for row in results["filtered"]] == ["cooking"] * 3
    engine.sql("BEGIN")
    engine.sql("UPDATE notes SET embedding = $1 WHERE id = 6", [uqa.vector([1, 0, 0, 0])])
    private_rows = knn(engine)
    assert (private_rows[0]["id"], private_rows[0]["_score"]) == (6, 1)
    engine.sql("ROLLBACK")
    results["after_rollback"] = knn(engine)
    assert results["after_rollback"] == results["exact"]
    engine.sql("UPDATE notes SET embedding = $1 WHERE id = 6", [uqa.vector([1, 0, 0, 0])])
    results["after_commit"] = knn(engine, 6)
    assert [row["id"] for row in results["after_commit"]] == [6, 1, 3, 2, 4, 5]
    assert (results["after_commit"][0]["id"], results["after_commit"][0]["_score"]) == (6, 1)
    return results


def knn(engine: object, k: int = 3) -> list:
    return engine.sql(
        "SELECT id, title, topic, _score FROM notes "
        "WHERE knn_match(embedding, $1, $2) ORDER BY _score DESC, id",
        [uqa.vector([1, 0, 0, 0]), k],
    ).rows


if __name__ == "__main__":
    main()
