//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retained notification registry pool cannot change its physical database source.

use crate::{ManagedConnection, SQLiteError};
use std::path::Path;

fn database(path: &Path, value: &str) -> ManagedConnection {
    let connection = ManagedConnection::open_auxiliary(path, None).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute_batch("CREATE TABLE source_marker(value TEXT NOT NULL)")?;
            sqlite.execute("INSERT INTO source_marker VALUES (?1)", [value])?;
            Ok(())
        })
        .unwrap();
    connection
}

fn replace(path: &Path, saved: &Path) {
    let replacement = path.with_extension("replacement");
    drop(database(&replacement, "replacement"));
    std::fs::rename(path, saved).unwrap();
    std::fs::rename(replacement, path).unwrap();
}

#[test]
fn source_replacement_is_rejected_before_a_new_pool_connection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let connection = database(&path, "original");
    let retained = connection.lease_connection().unwrap();
    replace(&path, &directory.path().join("saved.db"));
    let original: String = retained
        .query_row("SELECT value FROM source_marker", [], |row| row.get(0))
        .unwrap();
    assert_eq!(original, "original");
    let replacement = connection.lease_connection().map(|sqlite| {
        sqlite
            .query_row("SELECT value FROM source_marker", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
    });
    assert!(
        matches!(replacement, Err(SQLiteError::DatabaseSourceChanged)),
        "a retained pool switched physical source: {replacement:?}"
    );
}

#[test]
fn missing_source_is_not_recreated_for_an_additional_connection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let connection = database(&path, "original");
    let _retained = connection.lease_connection().unwrap();
    std::fs::rename(&path, directory.path().join("saved.db")).unwrap();
    let result = connection.lease_connection().map(|_| ());
    assert!(
        matches!(result, Err(SQLiteError::DatabaseSourceChanged)),
        "a removed source was reopened: {result:?}"
    );
    assert!(!path.exists(), "pool growth recreated the missing source");
}

#[test]
fn source_replacement_is_rejected_before_the_first_change_monitor_read() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let connection = database(&path, "original");
    replace(&path, &directory.path().join("saved.db"));
    assert!(
        matches!(
            connection.data_version(),
            Err(SQLiteError::DatabaseSourceChanged)
        ),
        "the change monitor attached to a replacement source"
    );
}

#[test]
fn source_change_is_terminal_even_when_the_original_path_is_restored() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let saved = directory.path().join("saved.db");
    let connection = database(&path, "original");
    replace(&path, &saved);
    let rejected = connection.with(|_| Ok(())).is_err();
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(saved, &path).unwrap();
    assert!(
        rejected,
        "even a cached connection must observe source invalidation"
    );
    assert!(
        connection.with(|_| Ok(())).is_err(),
        "a failed source must require explicit reopening"
    );
    drop(connection);
    let reopened = ManagedConnection::open_auxiliary(&path, None).unwrap();
    let marker = reopened
        .with(|sqlite| {
            Ok(
                sqlite.query_row("SELECT value FROM source_marker", [], |row| {
                    row.get::<_, String>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(marker, "original");
}

#[test]
fn an_existing_monitor_rejects_a_replaced_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let connection = database(&path, "original");
    assert!(connection.data_version().unwrap().is_some());
    replace(&path, &directory.path().join("saved.db"));
    assert!(matches!(
        connection.new_session().data_version(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
}

#[rstest::rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, true)]
fn file_and_encrypted_pools_retain_the_original_source(
    #[case] auxiliary: bool,
    #[case] encrypted: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let connection = if auxiliary {
        ManagedConnection::open_auxiliary(
            &path,
            Some(uqa_storage::StorageEncryptionKey::new(
                "retained-source-key",
            )),
        )
    } else if encrypted {
        ManagedConnection::open_encrypted(&path, "retained-source-key")
    } else {
        ManagedConnection::open(&path)
    }
    .unwrap();
    connection
        .with(|sqlite| {
            Ok(
                sqlite
                    .execute_batch("CREATE TABLE marker(value); INSERT INTO marker VALUES (1)")?,
            )
        })
        .unwrap();
    let lease = connection.lease_connection().unwrap();
    connection
        .new_session()
        .with(|sqlite| Ok(sqlite.execute_batch("INSERT INTO marker VALUES (2)")?))
        .unwrap();
    drop(lease);
    connection.vacuum().unwrap();
    let count = connection
        .with(|sqlite| {
            Ok(sqlite.query_row("SELECT COUNT(*) FROM marker", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert_eq!(count, 2);
    let retained = connection.lease_connection().unwrap();
    replace(&path, &directory.path().join("saved.db"));
    assert!(matches!(
        connection.new_session().lease_connection(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
    assert!(matches!(
        connection.data_version(),
        Err(SQLiteError::DatabaseSourceChanged)
    ));
    // Cleanup retains access to the original lease even after admission closes.
    drop(retained);
    drop(connection);
}

#[test]
fn retargeting_an_opening_symlink_does_not_select_another_database() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original.db");
    let other = directory.path().join("other.db");
    let alias = directory.path().join("alias.db");
    drop(database(&original, "original"));
    drop(database(&other, "other"));
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let connection = ManagedConnection::open_auxiliary(&alias, None).unwrap();
    let _retained = connection.lease_connection().unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    let next = connection.lease_connection().unwrap();
    let value: String = next
        .query_row("SELECT value FROM source_marker", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "original");
    assert_eq!(
        connection.database_path(),
        Some(original.canonicalize().unwrap().as_path())
    );
}

#[test]
fn closing_a_pool_preserves_another_pools_native_process_lock() {
    const CHILD_PATH: &str = "UQA_SOURCE_IDENTITY_LOCK_PROBE";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let connection = rusqlite::Connection::open_with_flags(
            std::path::PathBuf::from(path),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap();
        connection.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert!(matches!(
            connection.execute_batch("BEGIN IMMEDIATE"),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy
        ));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("locks.db");
    let first = database(&path, "original");
    let other = ManagedConnection::open_auxiliary(&path, None).unwrap();
    let writer = first.lease_connection().unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    drop(other);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection::tests::source_identity::closing_a_pool_preserves_another_pools_native_process_lock",
            "--nocapture",
        ])
        .env(CHILD_PATH, &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child lock probe failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn an_unpublished_compressed_source_preserves_its_initial_monitor_behavior() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("initial-compressed.db");
    let connection =
        ManagedConnection::open_compressed(&path, crate::SQLiteCompressionOptions::default())
            .unwrap();
    assert!(connection.data_version().unwrap().is_some());
    connection
        .with(|sqlite| Ok(sqlite.execute_batch("CREATE TABLE initialized(value)")?))
        .unwrap();
    assert!(connection.new_session().data_version().unwrap().is_some());
}
