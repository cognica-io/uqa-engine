//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-owned resources must expire without waiting for another database to open.

use super::*;

fn persistent(provider: usize, path: &std::path::Path) -> Engine {
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
        3 => native_storage::legacy_engine(path),
        _ => unreachable!(),
    }
}

fn assert_released(engine: Engine) {
    let provider = Arc::downgrade(engine.storage.provider.as_ref().unwrap());
    let backend = Arc::downgrade(engine.storage.backend.as_ref().unwrap());
    let session = Arc::downgrade(&engine.session);
    let catalog = Arc::downgrade(&engine.durable);
    let tables = Arc::downgrade(&engine.storage.tables);
    let statistics = Arc::downgrade(&engine.statistics);
    let notifications = Arc::downgrade(&engine.notification_hub);
    let locks = Arc::downgrade(&engine.row_locks);
    drop(engine);
    assert!(
        session.upgrade().is_none(),
        "session survived its last owner"
    );
    assert!(
        catalog.upgrade().is_none(),
        "catalog survived its last owner"
    );
    assert!(
        tables.upgrade().is_none(),
        "tables survived their last owner"
    );
    assert!(
        statistics.upgrade().is_none(),
        "maintenance survived its last owner"
    );
    assert!(
        notifications.upgrade().is_none(),
        "notifications survived their last owner"
    );
    assert!(locks.upgrade().is_none(), "locks survived their last owner");
    assert!(
        backend.upgrade().is_none(),
        "backend survived its last owner"
    );
    assert!(
        provider.upgrade().is_none(),
        "provider survived its last owner"
    );
}

#[test]
fn dropping_unfinished_transactions_releases_fixed_graph_and_cursor_snapshots() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent(provider, &directory.path().join("unfinished.db"));
        engine
            .sql(
                "CREATE TABLE t(id integer); INSERT INTO t VALUES (1), (2)",
                &[],
            )
            .unwrap();
        engine.create_graph("retained").unwrap();
        engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM t; SAVEPOINT nested; DECLARE pending CURSOR FOR SELECT * FROM t; FETCH 1 FROM pending", &[]).unwrap();
        assert_released(engine);
    }
}

#[test]
fn dropping_holdable_cursors_releases_their_committed_snapshot() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent(provider, &directory.path().join("holdable.db"));
        engine
            .sql(
                "CREATE TABLE t(id integer); INSERT INTO t VALUES (1), (2)",
                &[],
            )
            .unwrap();
        engine.create_graph("retained").unwrap();
        engine.sql("BEGIN; DECLARE held CURSOR WITH HOLD FOR SELECT * FROM t; FETCH 1 FROM held; COMMIT", &[]).unwrap();
        assert_released(engine);
    }
}

#[test]
fn dropping_session_deadlines_releases_their_termination_handles() {
    for provider in 0..4 {
        for transaction in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let engine = persistent(provider, &directory.path().join("deadlines.db"));
            engine.sql("SET idle_session_timeout = '1h'; SET idle_in_transaction_session_timeout = '1h'; SET transaction_timeout = '2h'", &[]).unwrap();
            if transaction {
                engine.sql("BEGIN; SELECT 1", &[]).unwrap();
            }
            assert_released(engine);
        }
    }
}

#[test]
fn dropping_a_parent_preserves_only_the_independent_sessions_resources() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent(provider, &directory.path().join("sessions.db"));
        engine
            .sql("CREATE TABLE t(id integer); INSERT INTO t VALUES (1)", &[])
            .unwrap();
        let sibling = engine.new_session().unwrap();
        let parent = Arc::downgrade(&engine.session);
        drop(engine);
        assert!(parent.upgrade().is_none());
        assert_eq!(
            sibling.sql("SELECT id FROM t", &[]).unwrap().rows[0]["id"],
            Value::Int(1)
        );
        assert_released(sibling);
    }
}
