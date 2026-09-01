//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn pg18_sql_cursor_zero_movement_rules_match_postgresql() {
    let engine = Engine::new();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE zero_position NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    let zero = engine
        .sql("FETCH FORWARD 0 FROM zero_position", &[])
        .unwrap();
    assert_eq!(zero.columns, ["x"]);
    assert_eq!(zero.column_types, [Some(ColumnType::Integer)]);
    assert!(zero.rows.is_empty());
    assert!(engine
        .sql("FETCH ABSOLUTE 0 FROM zero_position", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("MOVE BACKWARD 0 FROM zero_position", &[])
            .unwrap()
            .affected_rows,
        0
    );
    engine.sql("FETCH 1 FROM zero_position", &[]).unwrap();
    assert_eq!(
        engine
            .sql("MOVE RELATIVE 0 FROM zero_position", &[])
            .unwrap()
            .affected_rows,
        1
    );
    let refetch = engine
        .sql("FETCH RELATIVE 0 FROM zero_position", &[])
        .unwrap_err();
    assert_eq!(refetch.sqlstate(), Some("55000"), "{refetch}");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE negative_absolute NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    let backward_absolute = engine
        .sql("FETCH ABSOLUTE -1 FROM negative_absolute", &[])
        .unwrap_err();
    assert_eq!(
        backward_absolute.sqlstate(),
        Some("55000"),
        "{backward_absolute}"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn pg18_sql_cursor_no_scroll_and_locking_rules_match_postgresql() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE cursor_lock_rows (x INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO cursor_lock_rows VALUES (1), (2)", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE exhausted NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    engine.sql("FETCH 3 FROM exhausted", &[]).unwrap();
    assert!(engine
        .sql("FETCH FORWARD 0 FROM exhausted", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("MOVE FORWARD 0 FROM exhausted", &[])
            .unwrap()
            .affected_rows,
        0
    );
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE forward_only NO SCROLL CURSOR FOR SELECT x FROM (VALUES (1), (2), (3)) AS rows(x) ORDER BY x",
            &[],
        )
        .unwrap();
    engine.sql("FETCH 2 FROM forward_only", &[]).unwrap();
    let backward = engine
        .sql("FETCH BACKWARD FROM forward_only", &[])
        .unwrap_err();
    assert_eq!(backward.sqlstate(), Some("55000"), "{backward}");
    assert_eq!(backward.to_string(), "cursor can only scan forward");
    engine.sql("ROLLBACK", &[]).unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "DECLARE locking_cursor CURSOR FOR SELECT x FROM cursor_lock_rows ORDER BY x FOR UPDATE",
            &[],
        )
        .unwrap();
    engine.sql("FETCH FROM locking_cursor", &[]).unwrap();
    let backward = engine
        .sql("FETCH BACKWARD FROM locking_cursor", &[])
        .unwrap_err();
    assert_eq!(backward.sqlstate(), Some("55000"), "{backward}");
    engine.sql("ROLLBACK", &[]).unwrap();

    for sql in [
        "DECLARE held_lock CURSOR WITH HOLD FOR SELECT x FROM cursor_lock_rows FOR UPDATE",
        "DECLARE scrolling_lock SCROLL CURSOR FOR SELECT x FROM cursor_lock_rows FOR UPDATE",
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_projection_of_an_unknown_column_reports_undefined_column() {
    let eng = Engine::new();
    let error = eng.sql("SELECT trans_foo", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"), "{error}");
}

#[test]
fn pg18_plpgsql_return_accepts_query_shaped_expressions() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE plpgsql_return_source (a SMALLINT)", &[])
        .unwrap();
    eng.sql("INSERT INTO plpgsql_return_source VALUES (4), (9)", &[])
        .unwrap();
    eng.sql(
        "CREATE FUNCTION max_plpgsql_return_source() RETURNS SMALLINT LANGUAGE plpgsql AS 'BEGIN RETURN max(a) FROM plpgsql_return_source; END' STABLE",
        &[],
    )
    .unwrap();
    let result = eng
        .sql("SELECT max_plpgsql_return_source() AS maximum", &[])
        .unwrap();
    assert_eq!(result.rows[0]["maximum"], uqa_core::Value::Int(9));
}

#[test]
fn pg18_stable_and_volatile_routines_use_statement_and_command_visibility() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE routine_visibility (a SMALLINT, ordering INTEGER); INSERT INTO routine_visibility VALUES (56, 1), (100, 2), (0, 3), (42, 4), (777, 5)",
        &[],
    )
    .unwrap();

    for (language, body) in [
        ("sql", "SELECT max(a) FROM routine_visibility"),
        (
            "plpgsql",
            "BEGIN RETURN max(a) FROM routine_visibility; END",
        ),
    ] {
        eng.sql(
            &format!(
                "CREATE OR REPLACE FUNCTION max_routine_visibility() RETURNS SMALLINT LANGUAGE {language} AS '{body}' STABLE"
            ),
            &[],
        )
        .unwrap();
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql(
            "UPDATE routine_visibility SET a = max_routine_visibility() + 10 WHERE a > 0",
            &[],
        )
        .unwrap();
        let stable = eng
            .sql("SELECT a FROM routine_visibility ORDER BY ordering", &[])
            .unwrap();
        assert_eq!(integer_column(&stable, "a"), [787, 787, 0, 787, 787]);
        eng.sql("ROLLBACK", &[]).unwrap();

        eng.sql(
            &format!(
                "CREATE OR REPLACE FUNCTION max_routine_visibility() RETURNS SMALLINT LANGUAGE {language} AS '{body}' VOLATILE"
            ),
            &[],
        )
        .unwrap();
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql(
            "UPDATE routine_visibility SET a = max_routine_visibility() + 10 WHERE a > 0",
            &[],
        )
        .unwrap();
        let volatile = eng
            .sql("SELECT a FROM routine_visibility ORDER BY ordering", &[])
            .unwrap();
        assert_eq!(integer_column(&volatile, "a"), [787, 797, 0, 807, 817]);
        eng.sql("ROLLBACK", &[]).unwrap();
    }
}
