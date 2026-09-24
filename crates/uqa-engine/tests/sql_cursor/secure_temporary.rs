//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained public cursors and cancelled mutations keep temporary values protected across providers.

#[path = "secure_temporary/process.rs"]
mod process;

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::VersionedPersistence;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage_sqlite::{
    ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStorage, SQLiteStorageProvider,
};

const PATH_ENV: &str = "UQA_RETAINED_TEMP_TEST_DATABASE";
const MODE_ENV: &str = "UQA_RETAINED_TEMP_TEST_MODE";
const SECRET: &str = "private-retained-temp-secret-marker";
const KEY: &str = "retained temporary integration fixture";

fn open(path: &Path, mode: usize) -> (Engine, Arc<dyn VersionedPersistence>) {
    if mode == 8 {
        let provider = Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap());
        let records = Arc::new(provider.record_store().unwrap());
        return (Engine::from_persistent_provider(provider).unwrap(), records);
    }
    let connection = match mode % 4 {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, KEY),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        3 => ManagedConnection::open_compressed_encrypted(
            path,
            KEY,
            SQLiteCompressionOptions::default(),
        ),
        _ => unreachable!(),
    }
    .unwrap();
    if mode < 4 {
        let engine = Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(
            connection.clone(),
        )))
        .unwrap();
        let records = uqa_storage_sqlite::mvcc::SQLiteRecordStore::for_native(
            &connection,
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
        (engine, Arc::new(records))
    } else {
        let provider =
            Arc::new(SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap());
        let engine = Engine::from_persistent_provider(provider).unwrap();
        let records = uqa_storage_sqlite::mvcc::SQLiteRecordStore::new(&connection).unwrap();
        (engine, Arc::new(records))
    }
}

fn event(name: &str) {
    println!("retained-temp:{name}");
    std::io::stdout().flush().unwrap();
}

fn command() -> String {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "parent closed the fixture before release");
    line.trim().to_owned()
}

fn setup(engine: &Engine) {
    engine.sql(&format!("CREATE TABLE retained_temp_rows (id INTEGER PRIMARY KEY, secret TEXT UNIQUE, body TEXT); CREATE TABLE retained_temp_source (id INTEGER PRIMARY KEY, secret TEXT, body TEXT); INSERT INTO retained_temp_rows VALUES (1, '{SECRET}-1', 'before'), (2, '{SECRET}-2', 'before'), (3, '{SECRET}-3', 'before'); INSERT INTO retained_temp_source VALUES (1, '{SECRET}-1', 'new1'), (2, '{SECRET}-2', 'new2')"), &[]).unwrap();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
}

fn retained_cursors(engine: &Engine, records: &dyn VersionedPersistence) -> u64 {
    let reader = engine.new_session().unwrap();
    reader.sql("SET work_mem TO '1B'", &[]).unwrap();
    let cursor = reader
        .sql_cursor("SELECT id, secret FROM retained_temp_rows ORDER BY id", &[])
        .unwrap();
    assert!(cursor.spilled_to_disk());
    drop(reader);
    let control = StorageReadControl::with_limit(1 << 20);
    let old = records.snapshot(&control).unwrap();
    engine
        .sql(
            "UPDATE retained_temp_rows SET secret = 'post-capture' WHERE id = 3",
            &[],
        )
        .unwrap();
    records.reclaim_versions(&control).unwrap();
    event("cursor-held");
    assert_eq!(command(), "continue");
    let values: Vec<_> = cursor
        .flat_map(|batch| batch.unwrap().columns()[1].values.clone())
        .collect();
    assert_eq!(
        values,
        (1..=3)
            .map(|id| Value::Str(format!("{SECRET}-{id}")))
            .collect::<Vec<_>>()
    );
    drop(old);
    let released_versions = records.reclaim_versions(&control).unwrap();
    event("cursor-released");
    assert_eq!(command(), "continue");

    engine.sql("BEGIN; DECLARE retained_temp_cursor SCROLL CURSOR WITH HOLD FOR SELECT secret FROM retained_temp_rows ORDER BY id; COMMIT", &[]).unwrap();
    event("portal-held");
    assert_eq!(command(), "continue");
    let row = engine
        .sql("FETCH ABSOLUTE 2 FROM retained_temp_cursor", &[])
        .unwrap();
    assert_eq!(row.value_at(0, 0), Some(&Value::Str(format!("{SECRET}-2"))));
    engine.cancel();
    let error = engine
        .sql("FETCH NEXT FROM retained_temp_cursor", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{error}");
    engine.reset_cancellation();
    engine.sql("CLOSE ALL", &[]).unwrap();
    event("portal-released");
    assert_eq!(command(), "continue");
    released_versions
}

fn cancelled_mutation(engine: &Engine) {
    let cancellation = engine.cancellation_token();
    engine
        .register_scalar_function_with_options(
            "retained_temp_mutation_pause",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |values: &[Value]| {
                let [Value::Str(value)] = values else {
                    return Err(SQLError::Internal("fixture expects one text value".into()));
                };
                if value == "new2" {
                    // The first evaluated INSERT SELECT row and its exact conflict keys already belong to their temporary owners.
                    event("mutation-held");
                    assert_eq!(command(), "cancel");
                    cancellation.cancel();
                }
                Ok(Value::Str(value.clone()))
            },
        )
        .unwrap();
    engine
        .sql("BEGIN; SAVEPOINT before_temporary_mutation", &[])
        .unwrap();
    let error = engine.sql("INSERT INTO retained_temp_rows SELECT id, secret, body FROM retained_temp_source ORDER BY id ON CONFLICT (id) DO UPDATE SET body = retained_temp_mutation_pause(EXCLUDED.body)", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{error}");
    engine.reset_cancellation();
    engine
        .sql("ROLLBACK TO before_temporary_mutation", &[])
        .unwrap();
    assert_original_mutation(engine);
    engine.sql("ROLLBACK", &[]).unwrap();
    event("mutation-released");
    assert_eq!(command(), "continue");
}

fn assert_original_mutation(engine: &Engine) {
    let rows = engine
        .sql(
            "SELECT id, secret, body FROM retained_temp_rows ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 3);
    for (ordinal, row) in (1..=3).zip(rows.rows) {
        assert_eq!(row["id"], Value::Int(ordinal));
        let secret = if ordinal == 3 {
            "post-capture".to_owned()
        } else {
            format!("{SECRET}-{ordinal}")
        };
        assert_eq!(row["secret"], Value::Str(secret));
        assert_eq!(row["body"], Value::Str("before".into()));
    }
}

fn child(path: &Path, mode: usize) {
    let (engine, records) = open(path, mode);
    let control = StorageReadControl::with_limit(1 << 20);
    let independent_reader = records.snapshot(&control).unwrap();
    setup(&engine);
    let released_versions = retained_cursors(&engine, &*records);
    assert_eq!(released_versions, 0);
    cancelled_mutation(&engine);
    // Cursor release is not the final retention boundary. The independent reader pins earlier history, and the last Engine joins its automatic statistics worker before final reclamation.
    drop(engine);
    drop(independent_reader);
    let final_versions = records.reclaim_versions(&control).unwrap();
    assert!(final_versions > 0);
    assert_eq!(records.reclaim_versions(&control).unwrap(), 0);
    drop(records);
    event("closed");
}

#[test]
fn retained_temporary_data_is_encrypted_and_released_across_provider_modes() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        child(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
        );
        return;
    }
    for mode in 0..9 {
        process::verify(mode, false);
    }
}

#[test]
fn killed_encrypted_temporary_owner_preserves_committed_rows_without_plaintext_orphans() {
    for mode in [1, 3, 5, 7] {
        process::verify(mode, true);
    }
}
