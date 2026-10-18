//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLTableFunctionResult};

fn verify_streamed_document_fields(engine: &Engine) {
    let direct = engine.sql("SELECT value, tag FROM docs", &[]).unwrap();
    assert_eq!(direct.value_at(0, 0), Some(&Value::Int(7)));
    assert_eq!(direct.value_at(0, 1), Some(&Value::Str("seven".into())));
    for sql in [
        "WITH rows AS (SELECT value, tag FROM docs) SELECT * FROM rows",
        "WITH rows AS MATERIALIZED (SELECT value, tag FROM docs) SELECT * FROM rows",
        "WITH rows AS NOT MATERIALIZED (SELECT value, tag FROM docs) SELECT * FROM rows",
        "SELECT d.value, d.tag FROM (SELECT value, tag FROM docs) d",
        "SELECT d.value, d.tag FROM (WITH rows AS (SELECT value, tag FROM docs) SELECT * FROM rows) d",
    ] {
        let actual = engine.sql(sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(actual.columns, direct.columns, "{sql}");
        assert_eq!(actual.rows, direct.rows, "{sql}");
    }
    verify_streamed_document_values(engine);
    verify_streamed_document_boundaries(engine);
}

fn verify_streamed_document_values(engine: &Engine) {
    for (body, expected) in [
        (
            "SELECT d.value, d.missing AS absent FROM docs d",
            vec![Value::Int(7), Value::Null],
        ),
        (
            "SELECT d.n, d.label FROM (SELECT value, tag FROM docs) d(n,label)",
            vec![Value::Int(7), Value::Str("seven".into())],
        ),
        (
            "SELECT value + (random()*0)::int AS value FROM docs",
            vec![Value::Int(7)],
        ),
        (
            "SELECT value FROM docs WHERE EXISTS(SELECT 1)",
            vec![Value::Int(7)],
        ),
        (
            "SELECT d.value, k.known FROM docs d CROSS JOIN known k",
            vec![Value::Int(7), Value::Int(11)],
        ),
        (
            "SELECT value, known FROM docs d CROSS JOIN known k",
            vec![Value::Int(7), Value::Int(11)],
        ),
        (
            "SELECT _doc_id, value FROM docs",
            vec![Value::Int(1), Value::Int(7)],
        ),
    ] {
        for sql in [
            body.to_string(),
            format!("WITH rows AS MATERIALIZED ({body}) SELECT * FROM rows"),
            format!("SELECT * FROM ({body}) d"),
        ] {
            let actual = engine
                .sql(&sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(actual.rows.len(), 1, "{sql}");
            for (position, expected) in expected.iter().enumerate() {
                assert_eq!(actual.value_at(0, position), Some(expected), "{sql}");
            }
        }
    }
}

fn verify_streamed_document_boundaries(engine: &Engine) {
    let body = "SELECT _doc_id, _score, xmin, tableoid, value FROM docs";
    let direct = engine.sql(body, &[]).unwrap();
    for sql in [
        format!("WITH rows AS MATERIALIZED ({body}) SELECT * FROM rows"),
        format!("SELECT * FROM ({body}) d"),
    ] {
        let actual = engine
            .sql(&sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(actual.columns, direct.columns, "{sql}");
        assert_eq!(actual.rows, direct.rows, "{sql}");
    }
    let wildcard = engine.sql("SELECT * FROM docs", &[]).unwrap();
    let mut explicit_columns = wildcard.columns.clone();
    explicit_columns.push("value".into());
    for sql in [
        "SELECT *, value FROM docs",
        "SELECT * FROM (SELECT *, value FROM docs) d",
        "WITH rows AS MATERIALIZED (SELECT *, value FROM docs) SELECT * FROM rows",
    ] {
        let actual = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(actual.columns, explicit_columns, "{sql}");
        assert_eq!(
            actual.value_at(0, actual.columns.len() - 1),
            Some(&Value::Int(7)),
            "{sql}"
        );
    }
    // Independently checked on PostgreSQL 18.4: an explicitly declared empty table is closed, including materialized and aliased sources.
    for (sql, state) in [
        ("SELECT missing FROM empty", "42703"),
        (
            "WITH rows AS MATERIALIZED (SELECT missing FROM empty) SELECT * FROM rows",
            "42703",
        ),
        ("SELECT * FROM empty e(a)", "42P10"),
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap_err().sqlstate(),
            Some(state),
            "{sql}"
        );
    }
    let count = engine
        .sql("SELECT COUNT(*) FROM (SELECT * FROM empty) e", &[])
        .unwrap();
    assert_eq!(count.value_at(0, 0), Some(&Value::Int(1)));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn streamed_document_fields_survive_ctes_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("streamed-fields.db");
    let engine = open(provider, &path);
    engine.create_default_table("docs", Vec::new()).unwrap();
    engine
        .sql(
            "CREATE TABLE known(known integer); INSERT INTO known VALUES(11)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE empty(); INSERT INTO empty DEFAULT VALUES",
            &[],
        )
        .unwrap();
    engine
        .add_document(
            "docs",
            1,
            std::collections::BTreeMap::from([
                ("value".into(), Value::Int(7)),
                ("tag".into(), Value::Str("seven".into())),
            ]),
        )
        .unwrap();
    verify_streamed_document_fields(&engine);
    if provider != 0 {
        drop(engine);
        verify_streamed_document_fields(&open(provider, &path));
    }
}

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn verify_runtime_columns(engine: &Engine) {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_table_function("native_rows", move |_args: &[Value]| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(SQLTableFunctionResult::new(
                ["value", "tag"],
                vec![vec![Value::Int(7), Value::Str("seven".into())]],
            ))
        })
        .unwrap();
    for source in ["docs", "native_rows()"] {
        let columns = if source == "docs" { "value, tag" } else { "*" };
        for body in [
            format!("SELECT DISTINCT {columns} FROM {source}"),
            format!("WITH rows AS (SELECT DISTINCT {columns} FROM {source}) SELECT * FROM rows"),
        ] {
            for projection in ["d.value, d.tag", "d.*"] {
                let sql = format!("SELECT {projection} FROM ({body}) d");
                let before = calls.load(Ordering::SeqCst);
                let result = engine
                    .sql(&sql, &[])
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(result.rows.len(), 1, "{sql}");
                assert_eq!(result.rows[0]["value"], Value::Int(7), "{sql}");
                assert_eq!(result.rows[0]["tag"], Value::Str("seven".into()), "{sql}");
                assert_eq!(
                    calls.load(Ordering::SeqCst) - before,
                    usize::from(source == "native_rows()"),
                    "{sql}"
                );
            }
        }
    }
    // Schemaless document wildcards preserve the native scan's descriptor; they do not infer arbitrary document keys.
    let direct = engine.sql("SELECT DISTINCT * FROM docs", &[]).unwrap();
    let derived = engine
        .sql("SELECT * FROM (SELECT DISTINCT * FROM docs) d", &[])
        .unwrap();
    assert_eq!(derived.columns, direct.columns);
    assert_eq!(derived.rows, direct.rows);
    let renamed = engine
        .sql(
            "SELECT d.a, d.b FROM (SELECT DISTINCT * FROM native_rows()) d(a,b)",
            &[],
        )
        .unwrap();
    assert_eq!(renamed.columns, ["a", "b"]);
    assert_eq!(renamed.value_at(0, 0), Some(&Value::Int(7)));
    assert_eq!(renamed.value_at(0, 1), Some(&Value::Str("seven".into())));
    let before = calls.load(Ordering::SeqCst);
    let empty = engine
        .sql(
            "SELECT d.* FROM (SELECT DISTINCT * FROM native_rows()) d LIMIT 0",
            &[],
        )
        .unwrap();
    assert_eq!(empty.columns, ["value", "tag"]);
    assert!(empty.rows.is_empty());
    assert_eq!(
        calls.load(Ordering::SeqCst) - before,
        1,
        "runtime columns are required to bind the wildcard even without result rows"
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_runtime_columns_survive_qualification_aliases_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime-columns.db");
    let engine = open(provider, &path);
    engine.create_default_table("docs", Vec::new()).unwrap();
    engine
        .add_document(
            "docs",
            1,
            std::collections::BTreeMap::from([
                ("value".into(), Value::Int(7)),
                ("tag".into(), Value::Str("seven".into())),
            ]),
        )
        .unwrap();
    verify_runtime_columns(&engine);
    if provider != 0 {
        drop(engine);
        verify_runtime_columns(&open(provider, &path));
    }
}
