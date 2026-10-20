//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` input/trigger evaluation order and failure effects.

use super::{exec, Engine, TempDir, Value};

fn engine(provider: usize) -> (TempDir, Engine) {
    let directory = TempDir::new().unwrap();
    let engine = reopen(provider, &directory);

    exec(
        &engine,
        "CREATE SEQUENCE input_order_seq;
        CREATE TABLE input_order_base(id INTEGER, payload BIGINT);
        CREATE VIEW input_order_view AS SELECT * FROM input_order_base;
        CREATE FUNCTION input_order_trigger() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN NEW.payload := nextval('input_order_seq'); RETURN NEW; END $$;
        CREATE TRIGGER input_order_row INSTEAD OF INSERT ON input_order_view
        FOR EACH ROW EXECUTE FUNCTION input_order_trigger()",
    );
    (directory, engine)
}

fn reopen(provider: usize, directory: &TempDir) -> Engine {
    let path = directory.path().join("view-input.db");
    match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory_values(0, false)]
#[case::memory_select(0, true)]
#[case::sqlite_values(1, false)]
#[case::sqlite_select(1, true)]
#[case::sqlite_key_value_values(2, false)]
#[case::sqlite_key_value_select(2, true)]
#[case::redb_values(3, false)]
#[case::redb_select(3, true)]
fn trigger_view_insert_interleaves_input_and_row_trigger_calls(
    #[case] provider: usize,
    #[case] select: bool,
) {
    let (directory, engine) = engine(provider);
    let source = if select {
        "SELECT nextval('input_order_seq'),NULL::BIGINT FROM generate_series(1,3)"
    } else {
        "VALUES (nextval('input_order_seq'),NULL),(nextval('input_order_seq'),NULL),(nextval('input_order_seq'),NULL)"
    };
    let result = engine
        .sql(
            &format!("INSERT INTO input_order_view {source} RETURNING id,payload"),
            &[],
        )
        .unwrap();
    let actual = result
        .rows
        .iter()
        .map(|row| (row["id"].clone(), row["payload"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [(1, 2), (3, 4), (5, 6)].map(|(id, payload)| (Value::Int(id), Value::Int(payload)))
    );
    drop(engine);
    if provider != 0 {
        let reopened = reopen(provider, &directory);
        let result = reopened
            .sql("SELECT last_value FROM input_order_seq", &[])
            .unwrap();
        assert_eq!(result.rows[0]["last_value"], Value::Int(6));
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn trigger_view_insert_keeps_prior_sequence_effects_before_source_error(#[case] provider: usize) {
    let (directory, engine) = engine(provider);
    exec(&engine, "BEGIN; SAVEPOINT source_error");
    let error = engine
        .sql(
            "INSERT INTO input_order_view
        SELECT nextval('input_order_seq'),10/(3-n) FROM generate_series(1,3) AS s(n)
        RETURNING id,payload",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    exec(&engine, "ROLLBACK TO source_error");
    let result = engine
        .sql("SELECT last_value FROM input_order_seq", &[])
        .unwrap();
    assert_eq!(result.rows[0]["last_value"], Value::Int(5));
    exec(&engine, "ROLLBACK");
    drop(engine);
    if provider != 0 {
        let reopened = reopen(provider, &directory);
        let result = reopened
            .sql("SELECT last_value FROM input_order_seq", &[])
            .unwrap();
        assert_eq!(result.rows[0]["last_value"], Value::Int(5));
    }
}
