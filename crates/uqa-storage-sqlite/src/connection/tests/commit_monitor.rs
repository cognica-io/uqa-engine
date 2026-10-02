//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The commit monitor a pool shares between its sessions.

use crate::{ManagedConnection, SQLiteCompressionOptions};

#[test]
fn the_commit_monitor_changes_with_each_commit_of_another_connection_and_only_then() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("commit-monitor.db");
    let observer = ManagedConnection::open(&path).unwrap();
    let writer = observer.new_session();
    let other_process = ManagedConnection::open(&path).unwrap();
    let before = observer.commit_monitor_version().unwrap().unwrap();
    assert_eq!(observer.commit_monitor_version().unwrap(), Some(before));
    // Sessions of one pool read the same monitor.
    assert_eq!(writer.commit_monitor_version().unwrap(), Some(before));
    writer
        .with(|connection| {
            connection.execute_batch("CREATE TABLE committed (id INTEGER PRIMARY KEY)")?;
            Ok(())
        })
        .unwrap();
    let created = observer.commit_monitor_version().unwrap().unwrap();
    assert_ne!(created, before);
    // A read changes nothing, and an uncommitted write is not a commit.
    writer
        .with(|connection| {
            connection.query_row("SELECT count(*) FROM committed", [], |row| {
                row.get::<_, i64>(0)
            })?;
            Ok(())
        })
        .unwrap();
    other_process.begin_transaction().unwrap();
    other_process
        .with(|connection| {
            connection.execute("INSERT INTO committed (id) VALUES (1)", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(observer.commit_monitor_version().unwrap(), Some(created));
    other_process.commit_transaction().unwrap();
    let inserted = observer.commit_monitor_version().unwrap().unwrap();
    assert_ne!(inserted, created);
    assert_eq!(writer.commit_monitor_version().unwrap(), Some(inserted));

    // No monitor reads beside a rollback-journal writer or an in-memory database.
    let compressed = ManagedConnection::open_compressed(
        &directory.path().join("commit-monitor-compressed.db"),
        SQLiteCompressionOptions::default(),
    )
    .unwrap();
    assert_eq!(compressed.commit_monitor_version().unwrap(), None);
    let memory = ManagedConnection::open_in_memory().unwrap();
    assert_eq!(memory.commit_monitor_version().unwrap(), None);
}
