//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recovery across the actual durable-data and notification-publication boundary.

use super::*;
use std::path::Path;
use uqa_core::Value;

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
        _ => unreachable!(),
    }
}

#[rstest::rstest]
fn committed_notifications_survive_sender_loss_before_queue_publication(
    #[values(0, 1, 2)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let sender = open(provider, &directory.path().join("notifications.db"));
    sender.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    let listener = sender.new_session().unwrap();
    listener.sql("LISTEN committed_items", &[]).unwrap();
    let process_id = sender.backend_process_id();
    sender
        .sql(
            "BEGIN; INSERT INTO items VALUES (1); NOTIFY committed_items, 'one'",
            &[],
        )
        .unwrap();
    assert!(listener
        .sql("SELECT id FROM items", &[])
        .unwrap()
        .rows
        .is_empty());
    assert!(listener.take_sql_notifications().is_empty());

    let stack = sender.session.transactions.lock();
    let guard = sender
        .begin_notification_commit(true, stack.last().unwrap())
        .unwrap()
        .unwrap();
    sender
        .storage
        .backend
        .as_ref()
        .unwrap()
        .commit_transaction()
        .unwrap();
    drop(guard);
    drop(stack);
    drop(sender);

    let committed = listener.sql("SELECT id FROM items", &[]).unwrap();
    assert_eq!(committed.value_at(0, 0), Some(&Value::Int(1)));
    listener.poll_sql_notifications().unwrap();
    let delivered = listener.take_sql_notifications();
    assert_eq!(delivered.len(), 1, "committed publication was lost");
    assert_eq!(delivered[0].process_id, process_id);
    assert_eq!(delivered[0].channel, "committed_items");
    assert_eq!(delivered[0].payload, "one");
    listener.poll_sql_notifications().unwrap();
    assert!(listener.take_sql_notifications().is_empty());
}
