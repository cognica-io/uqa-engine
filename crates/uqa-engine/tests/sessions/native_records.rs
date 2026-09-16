//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine restore and sibling publication consume the native logical provider contract.

use super::{create_cross_store_table, scalar_int};
use std::{path::Path, sync::Arc};
use uqa_engine::{Engine, ScoringMode};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteStorageProvider,
};

const KEY: &str = "native session fixture";

fn native_engine(connection: ManagedConnection) -> Engine {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection))).unwrap()
}

#[test]
fn bound_native_engine_restores_and_publishes_sql_changes_to_sibling_sessions() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let engine = native_engine(connection);
    create_cross_store_table(&engine);
    let observer = engine.new_session().unwrap();
    engine
        .sql("INSERT INTO docs VALUES (1, 'alpha', ARRAY[1.0, 0.0])", &[])
        .unwrap();
    assert_eq!(
        scalar_int(&observer, "SELECT count(*) AS n FROM docs", "n"),
        1
    );
    engine.sql("BEGIN; UPDATE docs SET body = 'private' WHERE id = 1; SAVEPOINT keep; INSERT INTO docs VALUES (2, 'discarded', ARRAY[0.0, 1.0]); ROLLBACK TO SAVEPOINT keep; COMMIT", &[]).unwrap();
    assert_eq!(
        scalar_int(
            &observer,
            "SELECT count(*) AS n FROM docs WHERE body = 'private'",
            "n"
        ),
        1
    );
    assert_eq!(
        scalar_int(&observer, "SELECT count(*) AS n FROM docs", "n"),
        1
    );
    engine
        .sql("BEGIN; DELETE FROM docs; ROLLBACK", &[])
        .unwrap();
    assert_eq!(
        scalar_int(&observer, "SELECT count(*) AS n FROM docs", "n"),
        1
    );
}

#[derive(Clone, Copy)]
enum Mode {
    Plain,
    Encrypted,
    Compressed,
    CompressedEncrypted,
}

impl Mode {
    fn open(self, path: &Path) -> ManagedConnection {
        match self {
            Self::Plain => ManagedConnection::open(path),
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
}

fn assert_published(engine: &Engine) {
    assert_eq!(
        scalar_int(engine, "SELECT count(*) AS n FROM published_docs", "n"),
        1
    );
    assert_eq!(
        scalar_int(
            engine,
            "SELECT count(*) AS n FROM docs WHERE id = 1 AND body = 'published'",
            "n"
        ),
        1
    );
    assert_eq!(
        engine
            .search("docs", "body", "published", &ScoringMode::default(), 10)
            .unwrap()
            .len(),
        1
    );
    assert!(engine
        .search("docs", "body", "discarded", &ScoringMode::default(), 10)
        .unwrap()
        .is_empty());
    assert!(engine
        .search("docs", "body", "original", &ScoringMode::default(), 10)
        .unwrap()
        .is_empty());
    let neighbors = engine
        .knn_search("docs", "embedding", [0.0, 1.0], 10)
        .unwrap();
    assert_eq!(neighbors.len(), 1);
    assert!((neighbors[0].score - 1.0).abs() < 1e-6);
    assert_eq!(scalar_int(engine, "SELECT count(*) AS n FROM cypher('items', $$ MATCH (n) RETURN id(n) $$) AS result(id agtype)", "n"), 2);
}

fn assert_notifications(engine: &Engine, observer: &Engine, path: &Path, mode: Mode) {
    const CHANNEL: &str = "native_restore_channel";
    const PAYLOAD: &str = "native-restore-retained-secret-payload";
    observer.sql(&format!("LISTEN {CHANNEL}"), &[]).unwrap();
    observer.sql("BEGIN", &[]).unwrap();
    engine
        .sql(&format!("NOTIFY {CHANNEL}, '{PAYLOAD}'"), &[])
        .unwrap();
    assert!(observer.take_sql_notifications().is_empty());
    if matches!(mode, Mode::Encrypted | Mode::CompressedEncrypted) {
        let mut registry = path.as_os_str().to_owned();
        registry.push(".uqa-notification-state");
        let registry = std::path::PathBuf::from(registry);
        let raw = rusqlite::Connection::open(&registry).unwrap();
        assert!(raw
            .query_row("SELECT payload FROM queue_entries", [], |row| row
                .get::<_, String>(0))
            .is_err());
        drop(raw);
        let keyed = rusqlite::Connection::open(&registry).unwrap();
        keyed.pragma_update(None, "key", KEY).unwrap();
        let retained: String = keyed
            .query_row("SELECT payload FROM queue_entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, PAYLOAD);
        drop(keyed);
        let bytes = std::fs::read(registry).unwrap();
        for marker in [CHANNEL, PAYLOAD, KEY] {
            assert!(!bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()));
        }
    }
    observer.sql("COMMIT", &[]).unwrap();
    let notifications = observer.take_sql_notifications();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].channel, CHANNEL);
    assert_eq!(notifications[0].payload, PAYLOAD);
    engine
        .sql(
            &format!("BEGIN; NOTIFY {CHANNEL}, 'discarded'; ROLLBACK"),
            &[],
        )
        .unwrap();
    assert!(observer.take_sql_notifications().is_empty());
}

fn restore_legacy_and_reopen_native(mode: Mode) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.db");
    {
        let legacy = Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(
            mode.open(&path),
        )))
        .unwrap();
        create_cross_store_table(&legacy);
        legacy.create_graph("items").unwrap();
        legacy
            .add_graph_vertex(uqa_core::Vertex::new(1, "Item"), "items")
            .unwrap();
        legacy.sql("INSERT INTO docs VALUES (1, 'original', ARRAY[1.0, 0.0]); CREATE VIEW published_docs AS SELECT id, body FROM docs; CREATE SEQUENCE ids START 17", &[]).unwrap();
        assert_eq!(scalar_int(&legacy, "SELECT nextval('ids') AS n", "n"), 17);
    }
    {
        let engine = native_engine(mode.open(&path));
        let observer = engine.new_session().unwrap();
        assert_eq!(scalar_int(&engine, "SELECT nextval('ids') AS n", "n"), 18);
        engine.sql("BEGIN; UPDATE docs SET body = 'published', embedding = ARRAY[0.0, 1.0] WHERE id = 1", &[]).unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(2, "Item"), "items")
            .unwrap();
        engine
            .sql(
                "SAVEPOINT keep; INSERT INTO docs VALUES (2, 'discarded', ARRAY[1.0, 0.0])",
                &[],
            )
            .unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(3, "Item"), "items")
            .unwrap();
        engine
            .sql("ROLLBACK TO SAVEPOINT keep; COMMIT", &[])
            .unwrap();
        assert_published(&observer);
        engine
            .sql("BEGIN; DELETE FROM docs; ROLLBACK", &[])
            .unwrap();
        assert_published(&engine);
        assert_notifications(&engine, &observer, &path, mode);
    }
    let reopened = native_engine(mode.open(&path));
    assert_published(&reopened);
    assert_eq!(scalar_int(&reopened, "SELECT nextval('ids') AS n", "n"), 19);
    reopened
        .sql(
            "INSERT INTO docs VALUES (2, 'reopened', ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar_int(
            &reopened.new_session().unwrap(),
            "SELECT count(*) AS n FROM published_docs",
            "n"
        ),
        2
    );
}

#[test]
fn plain_native_engine_restores_legacy_state_and_reopens() {
    restore_legacy_and_reopen_native(Mode::Plain);
}

#[test]
fn encrypted_native_engine_restores_legacy_state_and_reopens() {
    restore_legacy_and_reopen_native(Mode::Encrypted);
}

#[test]
fn compressed_native_engine_restores_legacy_state_and_reopens() {
    restore_legacy_and_reopen_native(Mode::Compressed);
}

#[test]
fn compressed_encrypted_native_engine_restores_legacy_state_and_reopens() {
    restore_legacy_and_reopen_native(Mode::CompressedEncrypted);
}
