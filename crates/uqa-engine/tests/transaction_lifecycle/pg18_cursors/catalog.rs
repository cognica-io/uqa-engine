//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

#[path = "catalog/plpgsql.rs"]
mod plpgsql;

fn names(engine: &Engine) -> Vec<String> {
    let result = engine
        .sql("SELECT name FROM pg_cursors ORDER BY name", &[])
        .unwrap();
    (0..result.rows.len())
        .map(|index| match result.value_at(index, 0).unwrap() {
            Value::Str(name) => name.clone(),
            other => panic!("unexpected cursor name {other:?}"),
        })
        .collect()
}

#[test]
fn pg18_cursor_catalog_retains_declared_metadata_through_transaction_lifecycle() {
    let engine = Engine::new();
    let empty = engine.sql("SELECT * FROM pg_cursors", &[]).unwrap();
    assert_eq!(
        empty.columns,
        [
            "name",
            "statement",
            "is_holdable",
            "is_binary",
            "is_scrollable",
            "creation_time"
        ]
    );
    assert_eq!(
        empty.column_types,
        vec![
            Some(uqa_sql::ColumnType::Text),
            Some(uqa_sql::ColumnType::Text),
            Some(uqa_sql::ColumnType::Boolean),
            Some(uqa_sql::ColumnType::Boolean),
            Some(uqa_sql::ColumnType::Boolean),
            Some(uqa_sql::ColumnType::TimestampTz),
        ]
    );
    assert!(empty.rows.is_empty());
    engine.sql("BEGIN", &[]).unwrap();
    let declaration = "DECLARE alpha BINARY SCROLL CURSOR WITH HOLD FOR SELECT 1;";
    engine.sql(declaration, &[]).unwrap();
    let metadata = engine.sql("SELECT * FROM pg_cursors", &[]).unwrap();
    assert_eq!(metadata.value_at(0, 0), Some(&Value::Str("alpha".into())));
    assert_eq!(
        metadata.value_at(0, 1),
        Some(&Value::Str(declaration.into()))
    );
    for column in 2..5 {
        assert_eq!(metadata.value_at(0, column), Some(&Value::Bool(true)));
    }
    assert!(matches!(
        metadata.value_at(0, 5),
        Some(Value::Temporal(uqa_core::TemporalValue::TimestampTz { .. }))
    ));
    engine
        .sql("DECLARE plain CURSOR FOR SELECT 1", &[])
        .unwrap();
    engine
        .sql(
            "SAVEPOINT cursors; DECLARE doomed CURSOR FOR VALUES (2)",
            &[],
        )
        .unwrap();
    assert_eq!(names(&engine), ["alpha", "doomed", "plain"]);
    engine.sql("ROLLBACK TO cursors", &[]).unwrap();
    assert_eq!(names(&engine), ["alpha", "plain"]);
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(names(&engine), ["alpha"]);
    let held = engine.sql("SELECT * FROM pg_cursors", &[]).unwrap();
    for column in 0..6 {
        assert_eq!(held.value_at(0, column), metadata.value_at(0, column));
    }
    engine.sql("CLOSE alpha", &[]).unwrap();
    assert!(names(&engine).is_empty());
    engine
        .sql(
            "BEGIN; DECLARE rolled_back CURSOR WITH HOLD FOR SELECT 2; ROLLBACK",
            &[],
        )
        .unwrap();
    assert!(names(&engine).is_empty());
    engine
        .sql("DECLARE discarded CURSOR WITH HOLD FOR SELECT 2", &[])
        .unwrap();
    engine.sql("DISCARD ALL", &[]).unwrap();
    assert!(names(&engine).is_empty());
}

#[test]
fn pg18_cursor_catalog_is_live_during_its_own_fetch_and_hold_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("cursor-catalog.db")).unwrap();
    let sibling = engine.new_session().unwrap();
    engine
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE self_view NO SCROLL CURSOR FOR SELECT name FROM pg_cursors ORDER BY name",
            &[],
        )
        .unwrap();
    engine
        .sql("DECLARE later CURSOR FOR SELECT 1", &[])
        .unwrap();
    assert!(names(&sibling).is_empty());
    let fetched = engine.sql("FETCH ALL FROM self_view", &[]).unwrap();
    assert_eq!(fetched.rows.len(), 2);
    assert_eq!(fetched.value_at(0, 0), Some(&Value::Str("later".into())));
    assert_eq!(
        fetched.value_at(1, 0),
        Some(&Value::Str("self_view".into()))
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    engine.sql("BEGIN; DECLARE held_view NO SCROLL CURSOR WITH HOLD FOR SELECT name FROM pg_cursors; COMMIT", &[]).unwrap();
    let held = engine.sql("FETCH ALL FROM held_view", &[]).unwrap();
    assert_eq!(held.rows.len(), 1);
    assert_eq!(held.value_at(0, 0), Some(&Value::Str("held_view".into())));
    engine.sql("CLOSE ALL", &[]).unwrap();
    assert!(names(&engine).is_empty());
}

#[test]
fn pg18_cursor_catalog_keeps_the_complete_message_and_statement_start_time() {
    let engine = Engine::new();
    let message = "BEGIN; DECLARE in_batch CURSOR FOR SELECT 1; SELECT statement, creation_time = statement_timestamp() AS same_time FROM pg_cursors";
    for _ in 0..2 {
        let result = engine.sql(message, &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.value_at(0, 0), Some(&Value::Str(message.into())));
        assert_eq!(result.value_at(0, 1), Some(&Value::Bool(true)));
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_cursor_catalog_uses_builtin_identity_permissions_and_name_resolution() {
    let engine = Engine::new();
    let identity = engine
        .sql("SELECT 'pg_catalog.pg_cursors'::regclass::oid AS oid", &[])
        .unwrap();
    assert_eq!(integer_column(&identity, "oid"), [12077]);
    for (sql, action) in [
        ("INSERT INTO pg_cursors(name) VALUES ('x')", "insert into"),
        ("UPDATE pg_cursors SET name = 'x'", "update"),
        ("DELETE FROM pg_cursors", "delete from"),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{error}");
        assert_eq!(
            error.to_string(),
            format!("cannot {action} view \"pg_cursors\"")
        );
    }
    engine
        .sql("CREATE ROLE cursor_reader; SET ROLE cursor_reader", &[])
        .unwrap();
    assert!(names(&engine).is_empty());
    engine.sql("RESET ROLE; CREATE TABLE public.pg_cursors(name text); SET search_path = public, pg_catalog; INSERT INTO pg_cursors VALUES ('ordinary')", &[]).unwrap();
    assert_eq!(names(&engine), ["ordinary"]);
    assert!(engine
        .sql("SELECT * FROM pg_catalog.pg_cursors", &[])
        .unwrap()
        .rows
        .is_empty());
}
