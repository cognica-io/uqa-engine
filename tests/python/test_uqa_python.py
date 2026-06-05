#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import uqa


def test_sql_text_vector_tensor_and_cypher_surfaces() -> None:
    engine = uqa.Engine()
    result = engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, embedding VECTOR(3), chunks TENSOR(3))"
    )
    assert result.affected_rows == 0

    engine.sql(
        "INSERT INTO notes (id, title, body, embedding, chunks) VALUES ($1, $2, $3, $4, $5)",
        [
            1,
            "rust database",
            "rust query engine",
            uqa.vector([1.0, 0.0, 0.0]),
            uqa.tensor([[1.0, 0.0, 0.0], [0.8, 0.1, 0.0]]),
        ],
    )
    engine.sql(
        "INSERT INTO notes (id, title, body, embedding, chunks) VALUES ($1, $2, $3, $4, $5)",
        [
            2,
            "python client",
            "python package binding",
            uqa.vector([0.0, 1.0, 0.0]),
            uqa.tensor([[0.0, 1.0, 0.0]]),
        ],
    )
    engine.sql("CREATE INDEX notes_body_idx ON notes USING gin (body)")

    text = engine.sql(
        "SELECT id, _score FROM notes WHERE text_match(body, 'rust') ORDER BY _score DESC"
    )
    assert text.rows[0]["id"] == 1

    vector = engine.sql(
        "SELECT id, _score FROM notes WHERE knn_match(embedding, $1, 1)",
        [uqa.vector([1.0, 0.0, 0.0])],
    )
    assert vector.rows[0]["id"] == 1

    direct = engine.knn_search("notes", "embedding", [1.0, 0.0, 0.0], top_k=1)
    assert direct[0]["doc_id"] == 1

    cypher = engine.run_cypher(
        "social",
        "CREATE (:Person {name: $name}) RETURN $name AS name",
        {"name": "Ada"},
    )
    assert cypher.rows == [{"name": "Ada"}]


def test_persistent_open_and_batch(tmp_path) -> None:
    path = tmp_path / "uqa.db"
    engine = uqa.open(path)
    results = engine.sql_batch(
        [
            ("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", []),
            ("INSERT INTO docs (id, body) VALUES ($1, $2)", [1, "hello"]),
            ("SELECT body FROM docs WHERE id = $1", [1]),
        ]
    )
    assert results[-1].rows == [{"body": "hello"}]

    reopened = uqa.open(path)
    assert reopened.sql("SELECT count(*) AS n FROM docs").rows == [{"n": 1}]


def test_python_user_defined_functions() -> None:
    engine = uqa.Engine()
    engine.sql("CREATE TABLE samples (grp TEXT, val INTEGER)")
    engine.sql(
        "INSERT INTO samples (grp, val) VALUES ('a', 1), ('a', 2), ('b', 3)"
    )

    engine.register_scalar_function("py_prefix", lambda value: f"tag:{value}")
    scalar = engine.sql(
        "SELECT py_prefix(grp) AS tagged FROM samples WHERE val = 3"
    )
    assert scalar.rows == [{"tagged": "tag:b"}]

    def repeat_rows(label, times):
        return (["label", "idx"], [[label, idx] for idx in range(times)])

    engine.register_table_function("py_repeat_rows", repeat_rows)
    table = engine.sql(
        "SELECT label, idx FROM py_repeat_rows('row', 3) AS r(label, idx) ORDER BY idx"
    )
    assert table.rows == [
        {"idx": 0, "label": "row"},
        {"idx": 1, "label": "row"},
        {"idx": 2, "label": "row"},
    ]

    class SumSquares:
        def __init__(self):
            self.total = 0

        def observe(self, value):
            if value is not None:
                self.total += value * value

        def finish(self):
            return self.total

    engine.register_aggregate_function("py_sum_squares", SumSquares)
    aggregate = engine.sql(
        "SELECT grp, py_sum_squares(val) AS total FROM samples GROUP BY grp ORDER BY grp"
    )
    assert aggregate.rows == [
        {"grp": "a", "total": 5},
        {"grp": "b", "total": 9},
    ]
