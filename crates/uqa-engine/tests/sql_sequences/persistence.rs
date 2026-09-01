//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn relpersistence(engine: &Engine, sequence: &str) -> Value {
    engine
        .sql(
            &format!(
                "SELECT relpersistence FROM pg_catalog.pg_class WHERE oid = '{sequence}'::regclass"
            ),
            &[],
        )
        .unwrap()
        .rows[0]["relpersistence"]
        .clone()
}

#[test]
fn sequence_persistence_changes_match_postgresql_catalog_cache_and_errors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE logged_ids CACHE 3;
             CREATE UNLOGGED SEQUENCE unlogged_ids CACHE 3;
             CREATE TABLE owner_rows(id integer);
             CREATE SEQUENCE owned_ids OWNED BY owner_rows.id;
             CREATE TABLE serial_rows(id serial);
             CREATE TABLE identity_rows(id integer GENERATED ALWAYS AS IDENTITY);
             CREATE TABLE not_a_sequence(id integer)",
            &[],
        )
        .unwrap();

    assert_eq!(
        relpersistence(&engine, "logged_ids"),
        Value::Str("p".into())
    );
    assert_eq!(
        relpersistence(&engine, "unlogged_ids"),
        Value::Str("u".into())
    );

    assert_eq!(engine.nextval("logged_ids").unwrap(), 1);
    engine
        .sql("ALTER SEQUENCE logged_ids SET LOGGED", &[])
        .unwrap();
    assert_eq!(engine.nextval("logged_ids").unwrap(), 2);

    engine
        .sql("ALTER SEQUENCE logged_ids SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(engine.currval("logged_ids").unwrap(), 2);
    assert_eq!(engine.nextval("logged_ids").unwrap(), 4);
    assert_eq!(
        relpersistence(&engine, "logged_ids"),
        Value::Str("u".into())
    );

    engine
        .sql("ALTER TABLE logged_ids SET LOGGED", &[])
        .unwrap();
    assert_eq!(engine.currval("logged_ids").unwrap(), 4);
    assert_eq!(engine.nextval("logged_ids").unwrap(), 7);
    assert_eq!(
        relpersistence(&engine, "logged_ids"),
        Value::Str("p".into())
    );

    for sequence in ["owned_ids", "serial_rows_id_seq", "identity_rows_id_seq"] {
        engine
            .sql(&format!("ALTER SEQUENCE {sequence} SET UNLOGGED"), &[])
            .unwrap();
        assert_eq!(relpersistence(&engine, sequence), Value::Str("u".into()));
    }

    assert_sequence_persistence_errors(&engine);
}

fn assert_sequence_persistence_errors(engine: &Engine) {
    engine
        .sql("ALTER SEQUENCE IF EXISTS missing_ids SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".to_string(),
            "relation \"missing_ids\" does not exist, skipping".to_string()
        )]
    );
    engine
        .sql("ALTER TABLE IF EXISTS missing_table_ids SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".to_string(),
            "relation \"missing_table_ids\" does not exist, skipping".to_string()
        )]
    );
    for sql in [
        "ALTER SEQUENCE not_a_sequence SET UNLOGGED",
        "ALTER SEQUENCE IF EXISTS not_a_sequence SET UNLOGGED",
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some("42809"));
    }

    engine
        .sql("CREATE TEMP SEQUENCE temporary_ids", &[])
        .unwrap();
    let temporary = engine
        .sql("ALTER SEQUENCE temporary_ids SET LOGGED", &[])
        .unwrap_err();
    assert_eq!(temporary.sqlstate(), Some("42P16"));
    assert!(temporary.to_string().contains(
        "cannot change logged status of table \"temporary_ids\" because it is temporary"
    ));
    assert_eq!(
        engine
            .sql("ALTER TABLE temporary_ids SET UNLOGGED", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P16")
    );

    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        engine
            .sql("ALTER SEQUENCE IF EXISTS missing_ids SET UNLOGGED", &[])
            .unwrap_err()
            .sqlstate(),
        Some("25006")
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

fn assert_sequence_persistence_transactions(engine: &Engine) {
    engine
        .sql(
            "CREATE SEQUENCE persistence_alter_only CACHE 3;
             CREATE SEQUENCE persistence_value CACHE 3;
             CREATE SEQUENCE persistence_savepoint CACHE 3",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("persistence_alter_only").unwrap(), 1);
    assert_eq!(engine.nextval("persistence_value").unwrap(), 1);
    assert_eq!(engine.nextval("persistence_savepoint").unwrap(), 1);

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE persistence_alter_only SET UNLOGGED", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.nextval("persistence_alter_only").unwrap(), 2);
    assert_eq!(
        relpersistence(engine, "persistence_alter_only"),
        Value::Str("p".into())
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE persistence_value SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(engine.nextval("persistence_value").unwrap(), 4);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.currval("persistence_value").unwrap(), 4);
    assert_eq!(engine.nextval("persistence_value").unwrap(), 4);
    assert_eq!(
        relpersistence(engine, "persistence_value"),
        Value::Str("p".into())
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SAVEPOINT persistence_boundary", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE persistence_savepoint SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(engine.nextval("persistence_savepoint").unwrap(), 4);
    engine
        .sql("ROLLBACK TO SAVEPOINT persistence_boundary", &[])
        .unwrap();
    assert_eq!(engine.currval("persistence_savepoint").unwrap(), 4);
    assert_eq!(engine.nextval("persistence_savepoint").unwrap(), 4);
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        relpersistence(engine, "persistence_savepoint"),
        Value::Str("p".into())
    );

    engine
        .sql("ALTER SEQUENCE persistence_value SET UNLOGGED", &[])
        .unwrap();
    assert_eq!(
        relpersistence(engine, "persistence_value"),
        Value::Str("u".into())
    );
}

#[test]
fn sequence_persistence_changes_follow_transactions_in_memory() {
    assert_sequence_persistence_transactions(&Engine::new());
}

#[test]
fn sequence_persistence_changes_follow_transactions_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-persistence.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        assert_sequence_persistence_transactions(&engine);
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        relpersistence(&reopened, "persistence_alter_only"),
        Value::Str("p".into())
    );
    assert_eq!(
        relpersistence(&reopened, "persistence_value"),
        Value::Str("u".into())
    );
    assert_eq!(
        relpersistence(&reopened, "persistence_savepoint"),
        Value::Str("p".into())
    );
}
