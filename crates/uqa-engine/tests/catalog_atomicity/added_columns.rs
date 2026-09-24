//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Added-column metadata, defaults and key enforcement remain one publication.

use super::*;

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => {
            let connection = ManagedConnection::open(path).unwrap();
            let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
            let backend = Arc::new(SQLiteStorageBackend::new(connection));
            Engine::from_persistent_backends(catalog, backend).unwrap()
        }
        _ => unreachable!(),
    }
}

fn assert_original_columns(engine: &Engine) {
    let columns = engine.describe_table("left_t").unwrap().unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "v"]
    );
}

#[rstest::rstest]
fn unique_text_addition_preserves_default_and_enforcement_on_reopen(
    #[values(0, 1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("added-column.db");
    let engine = open(provider, &path);
    engine
        .sql(
            "CREATE TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine.sql("INSERT INTO left_t VALUES (1, 1)", &[]).unwrap();
    engine
        .sql(
            "ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'",
            &[],
        )
        .unwrap();
    drop(engine);

    let engine = open(provider, &path);
    let rows = engine.sql("SELECT id, v, k FROM left_t", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.value_at(0, 2), Some(&Value::Str("key1".into())));
    let error = engine
        .sql("INSERT INTO left_t (id, v) VALUES (2, 2)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"), "{error}");
    engine
        .sql("INSERT INTO left_t VALUES (2, 2, 'key2')", &[])
        .unwrap();
    let rows = engine.sql("SELECT k FROM left_t ORDER BY id", &[]).unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.value_at(1, 0), Some(&Value::Str("key2".into())));
}

#[rstest::rstest]
fn failed_unique_text_addition_restores_schema_and_rows(#[values(0, 1, 2, 3)] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-column.db");
    let engine = open(provider, &path);
    engine
        .sql(
            "CREATE TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO left_t VALUES (1, 1), (2, 2)", &[])
        .unwrap();
    let error = engine
        .sql(
            "ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"), "{error}");
    assert_original_columns(&engine);
    assert_eq!(
        engine.sql("SELECT * FROM left_t", &[]).unwrap().rows.len(),
        2
    );
    drop(engine);

    let engine = open(provider, &path);
    assert_original_columns(&engine);
    engine
        .sql("ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE", &[])
        .unwrap();
    let rows = engine.sql("SELECT k FROM left_t ORDER BY id", &[]).unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.value_at(0, 0), Some(&Value::Null));
    assert_eq!(rows.value_at(1, 0), Some(&Value::Null));
}

#[rstest::rstest]
fn unique_text_addition_savepoint_rollback_removes_all_field_state(
    #[values(0, 1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rolled-back-column.db");
    let engine = open(provider, &path);
    engine
        .sql(
            "CREATE TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER)",
            &[],
        )
        .unwrap();
    engine.sql("INSERT INTO left_t VALUES (1, 1)", &[]).unwrap();
    engine.sql("BEGIN; SAVEPOINT before_column", &[]).unwrap();
    engine
        .sql(
            "ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'",
            &[],
        )
        .unwrap();
    engine
        .sql("ROLLBACK TO before_column; COMMIT", &[])
        .unwrap();
    assert_original_columns(&engine);
    drop(engine);

    let engine = open(provider, &path);
    assert_original_columns(&engine);
    engine
        .sql(
            "ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'replacement'",
            &[],
        )
        .unwrap();
    let rows = engine.sql("SELECT k FROM left_t", &[]).unwrap();
    assert_eq!(rows.value_at(0, 0), Some(&Value::Str("replacement".into())));
}
