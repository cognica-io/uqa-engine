//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encryption follows the real provider and backend notification paths.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use tempfile::TempDir;
use uqa_engine::Engine;
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteStorageBackend,
};

use super::{exec, values};

pub(super) const KEY: &str = "notification-encryption-test-key";
pub(super) const PAYLOAD: &str = "retained-secret-notification-payload-marker";
const CHANNEL: &str = "confidential_notification_channel";

pub(super) fn registry_path(database: &Path) -> std::path::PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(".uqa-notification-state");
    path.into()
}

pub(super) fn assert_no_plaintext(database: &Path, markers: &[&str]) {
    let prefix = database.file_name().unwrap().to_str().unwrap();
    let mut inspected = 0;
    for entry in std::fs::read_dir(database.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_name().to_str().unwrap().starts_with(prefix) {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read notification-owned file: {error}"),
        };
        inspected += 1;
        for marker in markers {
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "plaintext marker found in {}",
                entry.path().display()
            );
        }
    }
    assert!(inspected >= 2, "inspect the database and retained registry");
}

#[test]
fn encrypted_notification_queue_and_listener_metadata_remain_encrypted() {
    for compressed in [false, true] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("protected.db");
        let listener = if compressed {
            Engine::open_compressed_encrypted(&database, KEY, SQLiteCompressionOptions::default())
                .unwrap()
        } else {
            Engine::open_encrypted(&database, KEY).unwrap()
        };
        let sender = Engine::open_auto(&database, Some(KEY)).unwrap();
        exec(&listener, &format!("LISTEN {CHANNEL}"));
        exec(&listener, "BEGIN");
        exec(&sender, &format!("NOTIFY {CHANNEL}, '{PAYLOAD}'"));
        assert!(listener.take_sql_notifications().is_empty());

        let registry = Connection::open(registry_path(&database)).unwrap();
        assert!(registry
            .query_row("SELECT count(*) FROM queue_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_err());
        drop(registry);
        let registry = Connection::open(registry_path(&database)).unwrap();
        registry.pragma_update(None, "key", KEY).unwrap();
        let retained: (String, String) = registry
            .query_row("SELECT channel, payload FROM queue_entries", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(retained, (CHANNEL.into(), PAYLOAD.into()));
        assert_no_plaintext(&database, &[CHANNEL, PAYLOAD, KEY]);
        exec(&listener, "COMMIT");
        assert_eq!(
            values(listener.take_sql_notifications()),
            [(CHANNEL.into(), PAYLOAD.into())]
        );
        exec(
            &sender,
            &format!("BEGIN; NOTIFY {CHANNEL}, 'discarded'; ROLLBACK"),
        );
        assert!(listener.take_sql_notifications().is_empty());
        drop(registry);
        drop(sender);
        drop(listener);
        assert_no_plaintext(&database, &[CHANNEL, PAYLOAD, KEY]);
        let reopened = Engine::open_auto(&database, Some(KEY)).unwrap();
        assert!(reopened.take_sql_notifications().is_empty());
    }
}

#[test]
fn encrypted_backend_factory_and_independent_sessions_protect_notifications() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("backend.db");
    let connection = ManagedConnection::open_encrypted(&database, KEY).unwrap();
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend = Arc::new(SQLiteStorageBackend::new(connection));
    let engine = Engine::from_persistent_backends(catalog, backend).unwrap();
    let sender = engine.new_session().unwrap();
    exec(&engine, &format!("LISTEN {CHANNEL}"));
    exec(&engine, "BEGIN");
    exec(&sender, &format!("NOTIFY {CHANNEL}, '{PAYLOAD}'"));
    assert_no_plaintext(&database, &[CHANNEL, PAYLOAD, KEY]);
    exec(&engine, "ROLLBACK");
    assert_eq!(
        values(engine.take_sql_notifications()),
        [(CHANNEL.into(), PAYLOAD.into())]
    );
}

#[test]
fn encrypted_open_rejects_and_preserves_an_existing_plaintext_registry() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("legacy.db");
    // Create the main database without Engine so the fixture represents the
    // old release's mismatched encrypted main file and plaintext registry.
    let main = ManagedConnection::open_encrypted(&database, KEY).unwrap();
    Catalog::open(main).unwrap();
    let path = registry_path(&database);
    let registry = Connection::open(&path).unwrap();
    registry
        .execute_batch("CREATE TABLE retained(payload TEXT)")
        .unwrap();
    registry
        .execute("INSERT INTO retained VALUES (?1)", [PAYLOAD])
        .unwrap();
    drop(registry);
    let original = std::fs::read(&path).unwrap();
    let error = Engine::open_encrypted(&database, KEY)
        .err()
        .expect("reject plaintext registry");
    assert!(
        error.to_string().contains("notification registry"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn encrypted_open_rejects_a_registry_with_a_different_key() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("mismatch.db");
    let main = ManagedConnection::open_encrypted(&database, KEY).unwrap();
    Catalog::open(main).unwrap();
    let path = registry_path(&database);
    let registry = ManagedConnection::open_encrypted(&path, "different-test-key").unwrap();
    registry
        .with(|connection| {
            connection.execute_batch("CREATE TABLE retained(payload TEXT)")?;
            connection.execute("INSERT INTO retained VALUES (?1)", [PAYLOAD])?;
            Ok(())
        })
        .unwrap();
    drop(registry);
    let original = std::fs::read(&path).unwrap();
    assert!(Engine::open_encrypted(&database, KEY).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(Engine::open_encrypted(&database, "wrong-main-key").is_err());
    assert!(Engine::open(&database).is_err());
}
