#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import pytest

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


def test_open_auto_and_format_detection(tmp_path) -> None:
    plain = tmp_path / "plain.db"
    assert uqa.detect_database_file(plain) == "missing"
    engine = uqa.open(plain)
    engine.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
    engine.close()
    assert uqa.detect_database_file(plain) == "sqlite"
    reopened = uqa.open_auto(plain)
    assert reopened.sql("SELECT count(*) AS n FROM t").rows == [{"n": 0}]
    reopened.close()

    encrypted = tmp_path / "encrypted.db"
    engine = uqa.open_auto(encrypted, key="uqa-python-test")
    engine.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
    engine.sql("INSERT INTO t (id) VALUES (1)")
    engine.close()
    assert uqa.detect_database_file(encrypted) == "unrecognized"
    reopened = uqa.open_auto(encrypted, key="uqa-python-test")
    assert reopened.sql("SELECT count(*) AS n FROM t").rows == [{"n": 1}]
    reopened.close()

    compressed = tmp_path / "compressed.db"
    engine = uqa.open_compressed(compressed)
    engine.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
    engine.close()
    assert uqa.detect_database_file(compressed) == "compressed"
    reopened = uqa.open_auto(compressed)
    assert reopened.sql("SELECT count(*) AS n FROM t").rows == [{"n": 0}]
    reopened.close()


def test_scoring_params_calibration_workflow() -> None:
    engine = uqa.Engine()
    engine.create_default_table("docs", ["body"])
    corpus = [
        "rust query engine with calibrated scoring",
        "python bindings for the rust engine",
        "vector search and text fusion",
        "probability calibrated hybrid retrieval",
        "postgresql compatible sql surface",
        "graph queries over the same storage",
    ]
    for doc_id, body in enumerate(corpus, start=1):
        engine.add_document("docs", doc_id, {"body": body})

    params = engine.estimate_scoring_params(
        "docs", "body", n_samples=8, tokens_per_query=2, seed=42
    )
    assert {"alpha", "beta", "base_rate"} <= set(params)

    assert engine.load_scoring_params("docs.body") == params
    assert engine.load_all_scoring_params()["docs.body"] == params

    calibrated = engine.search("docs", "body", "rust engine", top_k=5, scoring="bayesian")
    assert calibrated
    assert all(0.0 <= hit["score"] <= 1.0 for hit in calibrated)

    labels = [1 if "rust" in body else 0 for body in corpus]
    report = engine.calibration_report("docs", "body", "rust engine", labels)
    assert set(report) == {"ece", "brier", "log_loss", "bins"}
    assert report["bins"]

    learned = engine.learn_scoring_params("docs", "body", "rust engine", labels)
    assert {"alpha", "beta"} <= set(learned)

    raw = engine.search("docs", "body", "rust engine", top_k=1, scoring="bm25")
    engine.update_scoring_params("docs", "body", raw[0]["score"], 1)
    with pytest.raises(ValueError, match="label"):
        engine.update_scoring_params("docs", "body", raw[0]["score"], 2)

    assert engine.drop_scoring_params("docs.body") is True
    assert engine.drop_scoring_params("docs.body") is False

    # Hand-written parameters drive bayesian search: an extreme beta
    # pushes every posterior to ~0, which the identity calibration
    # (alpha=1, beta=0) could never produce for matching documents.
    engine.save_scoring_params(
        "docs.body", {"alpha": 2.0, "beta": 1000.0, "base_rate": 0.5}
    )
    suppressed = engine.search(
        "docs", "body", "rust engine", top_k=5, scoring="bayesian"
    )
    assert suppressed
    assert all(hit["score"] < 0.01 for hit in suppressed)


def test_sql_notices_and_function_depth_limit() -> None:
    engine = uqa.Engine()
    engine.sql("DO $$ BEGIN RAISE NOTICE 'v=% w=%% x=%', 1, 'two'; END $$")
    engine.sql("DO $$ BEGIN RAISE WARNING 'careful'; END $$")
    assert engine.take_sql_notices() == [
        ("NOTICE", "v=1 w=% x=two"),
        ("WARNING", "careful"),
    ]
    assert engine.take_sql_notices() == []

    engine.sql(
        """
        CREATE FUNCTION rec(n integer) RETURNS integer AS $$
        BEGIN
          IF n <= 0 THEN
            RETURN 0;
          END IF;
          RETURN rec(n - 1);
        END;
        $$ LANGUAGE plpgsql
        """
    )
    assert engine.sql_function_depth_limit() >= 1
    engine.set_sql_function_depth_limit(3)
    assert engine.sql_function_depth_limit() == 3
    with pytest.raises(RuntimeError, match="stack depth limit exceeded"):
        engine.sql("SELECT rec(10) AS v")
    engine.set_sql_function_depth_limit(64)
    assert engine.sql("SELECT rec(10) AS v").rows == [{"v": 0}]
    engine.set_sql_function_depth_limit(0)
    assert engine.sql_function_depth_limit() == 1
