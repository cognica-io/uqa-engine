#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Compare exact, HNSW, and IVF KNN through the Python binding."""

from __future__ import annotations

import json

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
    engine = uqa.Engine()
    try:
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
        results["filtered"] = engine.sql(
            "SELECT id, title, topic FROM notes "
            "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) "
            "AND topic = 'cooking' LIMIT 3"
        ).rows

        assert results["exact"][0]["id"] == 1
        assert results["hnsw"][0]["id"] == 1
        assert results["ivf"][0]["id"] == 1
        assert all(row["topic"] == "cooking" for row in results["filtered"])
        print(json.dumps(results, sort_keys=True))
    finally:
        engine.close()


def knn(engine: object) -> list:
    return engine.sql(
        "SELECT id, title, topic FROM notes "
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 3)"
    ).rows


if __name__ == "__main__":
    main()
