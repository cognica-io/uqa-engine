//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::SQLParam;

#[rstest::rstest]
#[case::memory(crate::tests::providers::Provider::Memory)]
#[case::sqlite_native(crate::tests::providers::Provider::SQLiteNative)]
#[case::sqlite_kv(crate::tests::providers::Provider::SQLiteKeyValue)]
#[case::redb(crate::tests::providers::Provider::Redb)]
fn nested_cursors_bind_each_transition_relation_without_reusing_outer_analysis(
    #[case] provider: crate::tests::providers::Provider,
) {
    const QUERY: &str =
        "SELECT id, payload, pg_typeof(payload)::text AS kind FROM changed_rows ORDER BY id";
    const NESTED_QUERY: &str =
        "SELECT payload, id, pg_typeof(payload)::text AS kind FROM changed_rows ORDER BY id";
    fn read(engine: &Engine, sql: &str) -> Result<Vec<Vec<Value>>, uqa_sql::SQLError> {
        let mut rows = Vec::new();
        for batch in engine.sql_cursor(sql, &[])? {
            for row in batch?.into_rows() {
                rows.push(vec![
                    row["id"].clone(),
                    row["payload"].clone(),
                    row["kind"].clone(),
                ]);
            }
        }
        Ok(rows)
    }
    fn assert_syntax_only(engine: &Engine) {
        let cached = engine
            .cached_sql_statement(NESTED_QUERY)
            .expect("nested parsed syntax remains reusable");
        assert!(
            cached.logical_plan.is_none(),
            "preliminary plan was retained"
        );
        assert!(cached.analyzed_plan.is_none());
        assert!(cached.optimized_plan.is_none());
    }
    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(provider.open(&directory.path().join("nested-cursor-cache.db")));
    let captured = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let output = Arc::clone(&captured);
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("read_transition_cursor", move |_args: &[Value]| {
            let engine = weak.upgrade().unwrap();
            let rows = read(&engine, QUERY)?;
            assert_eq!(read(&engine, NESTED_QUERY)?, rows);
            assert_syntax_only(&engine);
            let materialized = engine.sql(NESTED_QUERY, &[])?;
            let materialized: Vec<_> = materialized
                .rows
                .iter()
                .map(|row| {
                    vec![
                        row["id"].clone(),
                        row["payload"].clone(),
                        row["kind"].clone(),
                    ]
                })
                .collect();
            assert_eq!(materialized, rows);
            assert_syntax_only(&engine);
            output.lock().extend(rows);
            Ok(Value::Int(0))
        })
        .unwrap();
    engine.sql("CREATE TABLE changed_rows(id integer, payload boolean); INSERT INTO changed_rows VALUES(9,true); CREATE TABLE integer_rows(id integer, payload integer); CREATE TABLE text_rows(payload text, id integer); CREATE FUNCTION capture_cursor() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM read_transition_cursor(); RETURN NULL; END $$; CREATE TRIGGER capture_integer AFTER INSERT ON integer_rows REFERENCING NEW TABLE AS changed_rows FOR EACH STATEMENT EXECUTE FUNCTION capture_cursor(); CREATE TRIGGER capture_text AFTER INSERT ON text_rows REFERENCING NEW TABLE AS changed_rows FOR EACH STATEMENT EXECUTE FUNCTION capture_cursor()", &[]).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    let ordinary = vec![vec![
        Value::Int(9),
        Value::Bool(true),
        Value::Str("boolean".into()),
    ]];
    assert_eq!(read(&engine, QUERY).unwrap(), ordinary);
    let original = engine
        .cached_sql_analysis(QUERY)
        .expect("top-level analysis");
    for statement in [
        "INSERT INTO integer_rows VALUES(1,7)",
        "INSERT INTO text_rows VALUES('second',2)",
        "INSERT INTO integer_rows VALUES(3,11)",
    ] {
        engine.sql(statement, &[]).unwrap();
        assert!(original.ptr_eq(&engine.cached_sql_analysis(QUERY).unwrap()));
    }
    assert_eq!(
        *captured.lock(),
        vec![
            vec![Value::Int(1), Value::Int(7), Value::Str("integer".into())],
            vec![
                Value::Int(2),
                Value::Str("second".into()),
                Value::Str("text".into())
            ],
            vec![Value::Int(3), Value::Int(11), Value::Str("integer".into())],
        ]
    );
    assert_eq!(read(&engine, QUERY).unwrap(), ordinary);
    engine.sql("COMMIT", &[]).unwrap();
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_kv(Some(1))]
#[case::redb(Some(2))]
fn statement_analysis_survives_data_commits_and_rechecks_types_and_definitions(
    #[case] provider: Option<usize>,
) {
    let persistent = provider.map(crate::tests::relation_lock_support::sessions);
    let memory = Engine::new();
    let engine = persistent.as_ref().map_or(&memory, |(_, engine, _)| engine);
    engine
        .sql(
            "CREATE TABLE reused_analysis(id integer PRIMARY KEY, body text)",
            &[],
        )
        .unwrap();
    let sql = "INSERT INTO reused_analysis VALUES ($1, $2) RETURNING id, body";
    engine
        .sql(
            sql,
            &[
                SQLParam::Scalar(Value::Int(1)),
                SQLParam::Scalar(Value::Str("first".into())),
            ],
        )
        .unwrap();
    let analyzed = engine
        .cached_sql_analysis(sql)
        .expect("ordinary parameterized mutation is reusable");
    for id in 2..=8 {
        let result = engine
            .sql(
                sql,
                &[
                    SQLParam::Scalar(Value::Int(id)),
                    SQLParam::Scalar(Value::Str(format!("row-{id}"))),
                ],
            )
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(id));
        assert_eq!(result.rows[0]["body"], Value::Str(format!("row-{id}")));
        assert!(analyzed.ptr_eq(&engine.cached_sql_analysis(sql).unwrap()));
    }
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            sql,
            &[
                SQLParam::Scalar(Value::Int(20)),
                SQLParam::Scalar(Value::Str("undone".into())),
            ],
        )
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM reused_analysis", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(8)
    );
    assert!(engine
        .sql(
            sql,
            &[
                SQLParam::Scalar(Value::Bool(true)),
                SQLParam::Scalar(Value::Str("wrong type".into()))
            ]
        )
        .is_err());
    engine
        .sql(
            "BEGIN; ALTER TABLE reused_analysis RENAME COLUMN body TO renamed",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                sql,
                &[
                    SQLParam::Scalar(Value::Int(21)),
                    SQLParam::Scalar(Value::Str("wrong column".into()))
                ]
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    engine
        .sql(
            sql,
            &[
                SQLParam::Scalar(Value::Int(22)),
                SQLParam::Scalar(Value::Str("restored".into())),
            ],
        )
        .unwrap();
    if let Some((_, _, peer)) = &persistent {
        peer.sql(
            "ALTER TABLE reused_analysis RENAME COLUMN body TO renamed",
            &[],
        )
        .unwrap();
    } else {
        engine
            .sql(
                "ALTER TABLE reused_analysis RENAME COLUMN body TO renamed",
                &[],
            )
            .unwrap();
    }
    assert_eq!(
        engine
            .sql(
                sql,
                &[
                    SQLParam::Scalar(Value::Int(23)),
                    SQLParam::Scalar(Value::Str("obsolete".into()))
                ]
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
}

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_kv(Some(1))]
#[case::redb(Some(2))]
fn parsed_mutations_survive_data_changes_but_preserve_rollback_and_schema_checks(
    #[case] provider: Option<usize>,
) {
    let persistent = provider.map(crate::tests::relation_lock_support::sessions);
    let memory = Engine::new();
    let engine = persistent.as_ref().map_or(&memory, |(_, engine, _)| engine);
    engine
        .sql("CREATE TABLE cached_inserts(id integer PRIMARY KEY)", &[])
        .unwrap();
    let insert = "INSERT INTO cached_inserts(id) VALUES ($1) RETURNING id";
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(1))])
        .unwrap();
    let parsed = engine.cached_sql_statement(insert).unwrap().statement;
    for value in 2..=4 {
        let result = engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(value))])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(value));
        assert!(Arc::ptr_eq(
            &parsed,
            &engine.cached_sql_statement(insert).unwrap().statement
        ));
    }
    engine.begin().unwrap();
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(5))])
        .unwrap();
    engine.rollback().unwrap();
    assert_eq!(
        engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(3))])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(6))])
        .unwrap();
    let rows = engine
        .sql("SELECT id FROM cached_inserts ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        [1, 2, 3, 4, 6].map(Value::Int)
    );
    engine
        .sql(
            "ALTER TABLE cached_inserts RENAME COLUMN id TO renamed",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(7))])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(
        engine
            .sql("SELECT id FROM cached_inserts ORDER BY id", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
}
