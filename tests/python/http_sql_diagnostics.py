#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run against an isolated HTTP server supplied by its authenticated integration harness."""

import os
import uqa


def main():
    engine = uqa.HttpEngine(os.environ["UQA_HTTP_TEST_URL"], os.environ["UQA_HTTP_TEST_TOKEN"])
    engine.sql("CREATE TABLE py_diagnostic_private (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(3))")
    engine.sql("INSERT INTO py_diagnostic_private VALUES (0, 'py_diagnostic_private', $1)", [uqa.vector([1, 0, 0])])
    for sql, state, category, position in [
        ("SELECT * FROM missing_py_diagnostic_private", "42P01", "undefined_table", None),
        ("SELECT missing_py_diagnostic_private FROM py_diagnostic_private", "42703", "undefined_column", None),
        ("SELECT ) /* py_diagnostic_private */", "42601", "syntax", 8),
        ("INSERT INTO py_diagnostic_private VALUES (2, 'py_diagnostic_private', $1)", "22023", "vector_dimension_mismatch", None),
        ("SELECT id FROM py_diagnostic_private WHERE text_match(body, 'py_diagnostic_private')", "42804", "index_required", None),
    ]:
        try:
            params = [uqa.vector([1, 0])] if category == "vector_dimension_mismatch" else []
            engine.sql(sql, params)
        except uqa.HttpEngineError as error:
            assert error.diagnostic["sqlstate"] == state
            assert error.diagnostic["category"] == category
            assert error.diagnostic["position"] == position
            assert "py_diagnostic_private" not in str(error)
        else:
            raise AssertionError("expected SQL rejection")
        if sql.startswith("SELECT"):
            frames = list(engine.sql_stream(sql))
            assert len(frames) == 1
            assert frames[0]["type"] == "error"
            assert frames[0]["diagnostic"]["sqlstate"] == state
            assert frames[0]["diagnostic"]["category"] == category
            assert "py_diagnostic_private" not in frames[0]["message"]
    try:
        engine.sql_batch([
            ("INSERT INTO py_diagnostic_private (id) VALUES (1)", []),
            ("SELECT missing_py_diagnostic_private FROM py_diagnostic_private", []),
            ("INSERT INTO py_diagnostic_private (id) VALUES (3)", []),
        ])
    except uqa.HttpEngineError as error:
        assert error.diagnostic["statement_index"] == 1
        assert error.diagnostic["sqlstate"] == "42703"
    else:
        raise AssertionError("expected atomic batch rejection")
    assert engine.sql("SELECT id FROM py_diagnostic_private ORDER BY id").rows == [{"id": 0}]
    print("Python HTTP SQL diagnostics: PASS")


if __name__ == "__main__":
    main()
