//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `PostgreSQL` reference schedule requires B to commit before A ends its private write transaction.

use std::{path::Path, sync::mpsc, sync::Arc, thread, time::Duration};

use serde::Deserialize;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLError;
use uqa_storage_sqlite::{ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStorage};

const DEADLOCK_GUARD: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Oracle {
    setup: Vec<String>,
    observe: String,
    cases: Vec<Schedule>,
}

#[derive(Deserialize)]
struct Schedule {
    name: String,
    a_before: Vec<String>,
    b: Vec<String>,
    before_a_end: Vec<String>,
    a_finish: Vec<String>,
    after_a_end: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Native,
    KeyValue,
    NativeFile(FileMode),
    KeyValueFile(FileMode),
    Redb,
}

#[derive(Clone, Copy, Debug)]
enum FileMode {
    Encrypted,
    Compressed,
    CompressedEncrypted,
}

const KEY: &str = "concurrent writer fixture";

impl FileMode {
    fn connection(self, path: &Path) -> ManagedConnection {
        match self {
            Self::Encrypted => ManagedConnection::open_encrypted(path, KEY),
            Self::Compressed => {
                ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default())
            }
            Self::CompressedEncrypted => ManagedConnection::open_compressed_encrypted(
                path,
                KEY,
                SQLiteCompressionOptions::default(),
            ),
        }
        .unwrap()
    }

    fn engine(self, path: &Path) -> Engine {
        match self {
            Self::Encrypted => Engine::open_encrypted(path, KEY),
            Self::Compressed => Engine::open_compressed(path, SQLiteCompressionOptions::default()),
            Self::CompressedEncrypted => {
                Engine::open_compressed_encrypted(path, KEY, SQLiteCompressionOptions::default())
            }
        }
        .unwrap()
    }
}

impl Layout {
    fn open(self, path: &Path) -> Engine {
        match self {
            Self::Native => Engine::open(path).unwrap(),
            Self::KeyValue => Engine::from_persistent_provider(Arc::new(
                SQLiteKeyValueStorage::open(path).unwrap(),
            ))
            .unwrap(),
            Self::NativeFile(mode) => mode.engine(path),
            Self::KeyValueFile(mode) => Engine::from_persistent_provider(Arc::new(
                SQLiteKeyValueStorage::from_connection(mode.connection(path)).unwrap(),
            ))
            .unwrap(),
            Self::Redb => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(path).unwrap(),
            ))
            .unwrap(),
        }
    }
}

fn steps(engine: &Engine, statements: &[String]) -> Result<(), SQLError> {
    for statement in statements {
        engine.sql(statement, &[])?;
    }
    Ok(())
}

fn rows(engine: &Engine, statement: &str) -> Result<Vec<String>, SQLError> {
    Ok(engine
        .sql(statement, &[])?
        .rows
        .iter()
        .map(|row| {
            let (Value::Int(id), Value::Int(value)) = (&row["id"], &row["value"]) else {
                panic!("expected integer reference rows, found {row:?}");
            };
            format!("{id}|{value}")
        })
        .collect())
}

fn verify(layout: Layout, probe_before_finish: bool, independent_provider: bool) {
    let oracle: Oracle = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/concurrent_writes.expected.json"
    )))
    .unwrap();
    for case in &oracle.cases {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("concurrent.db");
        // redb has one physical database owner per file. Independent Engine instances share that owner while retaining separate logical sessions and runtime state.
        let redb = matches!(layout, Layout::Redb)
            .then(|| Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()));
        let a = redb.as_ref().map_or_else(
            || layout.open(&path),
            |provider| Engine::from_persistent_provider(provider.clone()).unwrap(),
        );
        steps(&a, &oracle.setup).unwrap();
        let b = if independent_provider {
            redb.map_or_else(
                || layout.open(&path),
                |provider| Engine::from_persistent_provider(provider).unwrap(),
            )
        } else {
            drop(redb);
            a.new_session().unwrap()
        };
        steps(&a, &case.a_before).unwrap();
        assert_eq!(a.transaction_depth(), 1);
        let cancel = b.cancellation_token();
        let (done, completed) = mpsc::channel();
        let b = thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let result = steps(&b, &case.b).and_then(|()| rows(&b, &oracle.observe));
                done.send(result).unwrap();
                b
            });
            let result = completed.recv_timeout(DEADLOCK_GUARD);
            if result.is_err() {
                cancel.cancel();
                a.sql("ROLLBACK", &[]).unwrap();
                drop(worker.join().unwrap());
                panic!(
                    "{layout:?} {}: B did not finish before A was released: {result:?}",
                    case.name
                );
            }
            let observed = result.unwrap().unwrap();
            assert_eq!(observed, case.before_a_end, "{layout:?} {}", case.name);
            assert_eq!(a.transaction_depth(), 1);
            // A's next READ COMMITTED command must preserve its own row and also see B's commit before A finishes or rolls back to its earlier savepoint.
            if probe_before_finish {
                assert_eq!(rows(&a, &oracle.observe).unwrap(), ["1|10", "2|20"]);
            }
            steps(&a, &case.a_finish).unwrap_or_else(|error| {
                panic!(
                    "{layout:?} {} (probe={probe_before_finish}): {error}",
                    case.name
                )
            });
            worker.join().unwrap()
        });
        assert_eq!(rows(&a, &oracle.observe).unwrap(), case.after_a_end);
        assert_eq!(rows(&b, &oracle.observe).unwrap(), case.after_a_end);
        drop((a, b));
        assert_eq!(
            rows(&layout.open(&path), &oracle.observe).unwrap(),
            case.after_a_end
        );
    }
}

#[test]
fn default_native_sqlite_writers_follow_the_postgresql_commit_schedule() {
    verify(Layout::Native, true, false);
}

#[test]
fn sqlite_key_value_writers_follow_the_postgresql_commit_schedule() {
    verify(Layout::KeyValue, true, false);
}

#[test]
fn redb_writers_follow_the_postgresql_commit_schedule() {
    verify(Layout::Redb, true, false);
}

#[test]
fn committing_without_an_intervening_read_preserves_independent_writes() {
    for layout in [Layout::Native, Layout::KeyValue, Layout::Redb] {
        verify(layout, false, false);
    }
}

#[test]
fn independent_engines_share_concurrent_commits_and_savepoint_undo() {
    for layout in [Layout::Native, Layout::KeyValue, Layout::Redb] {
        for probe in [false, true] {
            verify(layout, probe, true);
        }
    }
}

#[test]
fn encrypted_and_compressed_files_preserve_the_concurrent_sql_schedule() {
    for mode in [
        FileMode::Encrypted,
        FileMode::Compressed,
        FileMode::CompressedEncrypted,
    ] {
        for layout in [Layout::NativeFile(mode), Layout::KeyValueFile(mode)] {
            for probe in [false, true] {
                verify(layout, probe, false);
            }
        }
    }
}

#[test]
fn private_writers_use_autonomous_sequence_values_without_publishing_private_definitions() {
    for layout in [Layout::Native, Layout::KeyValue, Layout::Redb] {
        let directory = tempfile::tempdir().unwrap();
        let a = layout.open(&directory.path().join("sequence.db"));
        a.sql("CREATE TABLE records(id INT PRIMARY KEY, value INT); INSERT INTO records VALUES (1,0); CREATE SEQUENCE ids", &[]).unwrap();
        let b = a.new_session().unwrap();
        let value = |engine: &Engine, sql: &str| super::scalar_int(engine, sql, "n");
        a.sql("BEGIN; UPDATE records SET value=10 WHERE id=1", &[])
            .unwrap();
        assert_eq!(value(&a, "SELECT nextval('ids') AS n"), 1);
        assert_eq!(value(&b, "SELECT nextval('ids') AS n"), 2);
        assert_eq!(value(&a, "SELECT setval('ids',10,false) AS n"), 10);
        assert_eq!(value(&b, "SELECT nextval('ids') AS n"), 10);
        a.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(value(&b, "SELECT nextval('ids') AS n"), 11);

        a.sql("BEGIN; ALTER SEQUENCE ids RESTART WITH 100", &[])
            .unwrap();
        assert_eq!(value(&a, "SELECT nextval('ids') AS n"), 100);
        a.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(value(&b, "SELECT nextval('ids') AS n"), 12);

        a.sql("BEGIN; CREATE SEQUENCE private_ids START 50", &[])
            .unwrap();
        assert_eq!(value(&a, "SELECT nextval('private_ids') AS n"), 50);
        a.sql(
            "SAVEPOINT keep; ALTER SEQUENCE private_ids RESTART WITH 100",
            &[],
        )
        .unwrap();
        assert_eq!(value(&a, "SELECT nextval('private_ids') AS n"), 100);
        a.sql("ROLLBACK TO keep", &[]).unwrap();
        assert_eq!(value(&a, "SELECT nextval('private_ids') AS n"), 51);
        a.sql("COMMIT", &[]).unwrap();
        assert_eq!(value(&b, "SELECT nextval('private_ids') AS n"), 52);
    }
}
