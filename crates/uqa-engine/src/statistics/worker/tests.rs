//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Idle maintenance must not fetch record payloads or scan the catalog again.

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteStorageBackend};

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
        3 => {
            let connection = ManagedConnection::open(path).unwrap();
            Engine::from_persistent_backends(
                Arc::new(Catalog::open(connection.clone()).unwrap()),
                Arc::new(SQLiteStorageBackend::new(connection)),
            )
            .unwrap()
        }
        _ => unreachable!(),
    }
}

fn manual_worker(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

#[test]
fn session_admission_commits_and_policy_changes_do_not_signal_an_existing_worker() {
    let directory = tempfile::tempdir().unwrap();
    let engine = persistent(0, &directory.path().join("admission.db"));
    engine.sql("CREATE TABLE items (id INTEGER)", &[]).unwrap();
    engine.release_automatic_statistics_client();

    // Keep a live worker whose request channel is observable without a timer.
    // Its independent stop channel also releases it if an assertion unwinds.
    let (stop, stopped) = mpsc::sync_channel::<()>(1);
    let (requests, received) = mpsc::sync_channel(1);
    let cancellation = uqa_core::CancellationToken::new();
    *engine.statistics.automatic_statistics.control.lock() = Some(super::super::StatisticsWorker {
        sender: requests,
        cancellation: cancellation.clone(),
        thread: std::thread::spawn(move || {
            let _ = stopped.recv();
        }),
    });
    engine.start_automatic_statistics();
    let peer = engine.new_session().unwrap();
    peer.sql("SELECT 1; INSERT INTO items VALUES (1)", &[])
        .unwrap();
    engine.set_diskann_rebuild_policy(crate::DiskANNRebuildPolicy::default());
    assert_eq!(received.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(peer);
    stop.send(()).unwrap();
    drop(engine);
    assert!(cancellation.is_cancelled());
    assert_eq!(
        received.try_recv(),
        Ok(()),
        "the last client must still wake shutdown"
    );
}

#[test]
fn shutdown_interrupts_a_pending_maintenance_deadline() {
    let cancellation = uqa_core::CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let (requests, receiver) = mpsc::sync_channel(1);
    let (finished, completion) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        finished
            .send(wait_until(
                &receiver,
                &worker_cancellation,
                Instant::now() + Duration::from_secs(3600),
            ))
            .unwrap();
    });
    cancellation.cancel();
    let _ = requests.try_send(());
    assert!(!completion.recv_timeout(Duration::from_secs(10)).unwrap());
    worker.join().unwrap();
}

#[rstest::rstest]
fn unchanged_polls_skip_table_passes_but_peer_commits_and_age_still_refresh(
    #[values(0, 1, 2, 3)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = persistent(provider, &directory.path().join("deadlines.db"));
    manual_worker(&engine);
    engine
        .sql(
            "CREATE TABLE items (id INTEGER); INSERT INTO items VALUES (1); ANALYZE items",
            &[],
        )
        .unwrap();
    let peer = engine.new_session().unwrap();
    manual_worker(&peer);
    let mut cache = cache::MaintenanceCache::default();
    let now = now_ms();
    refresh_due_tables_at(&engine, &mut cache, now).unwrap();
    assert_eq!(cache.full_passes, 1);
    for offset in 1..=4 {
        refresh_due_tables_at(&engine, &mut cache, now + offset).unwrap();
    }
    assert_eq!(cache.full_passes, 1, "idle polls must not enumerate tables");

    peer.sql("INSERT INTO items VALUES (2)", &[]).unwrap();
    // Use an old durable timestamp so both the scheduler's explicit clock and
    // ANALYZE's ordinary clock can cross the deadline without sleeping.
    peer.begin().unwrap();
    let catalog = peer.storage.catalog.as_deref().unwrap();
    let key = MaintenanceState::key("public.items");
    let mut stored: serde_json::Value =
        serde_json::from_str(&catalog.get_metadata(&key).unwrap().unwrap()).unwrap();
    stored["dirty_since_ms"] = serde_json::json!(1);
    catalog.set_metadata(&key, &stored.to_string()).unwrap();
    peer.commit().unwrap();
    let state =
        MaintenanceState::load(engine.storage.catalog.as_deref().unwrap(), "public.items").unwrap();
    let deadline = state
        .next_due_at(false, 1, crate::statistics::value_size::FORMAT_VERSION)
        .unwrap();
    let completed = engine.automatic_statistics_status().completed;
    refresh_due_tables_at(&engine, &mut cache, deadline - 1).unwrap();
    assert_eq!(
        cache.full_passes, 2,
        "a peer commit invalidates the completed pass"
    );
    refresh_due_tables_at(&engine, &mut cache, deadline - 1).unwrap();
    assert_eq!(cache.full_passes, 2);
    assert_eq!(engine.automatic_statistics_status().completed, completed);
    refresh_due_tables_at(&engine, &mut cache, deadline).unwrap();
    assert_eq!(cache.full_passes, 3);
    assert_eq!(
        engine.automatic_statistics_status().completed,
        completed + 1
    );
    refresh_due_tables_at(&engine, &mut cache, deadline).unwrap();
    let settled = cache.full_passes;
    refresh_due_tables_at(&engine, &mut cache, deadline + 1).unwrap();
    assert_eq!(cache.full_passes, settled);

    peer.sql("CREATE TABLE events (id INTEGER)", &[]).unwrap();
    refresh_due_tables_at(&engine, &mut cache, deadline + 2).unwrap();
    assert_eq!(
        engine.automatic_statistics_status().completed,
        completed + 2
    );
    peer.sql("INSERT INTO items VALUES (3)", &[]).unwrap();
    engine.runtime.cancellation.cancel();
    assert!(refresh_due_tables_at(&engine, &mut cache, deadline + 3).is_err());
    engine.runtime.cancellation.reset();
    let failed = cache.full_passes;
    refresh_due_tables_at(&engine, &mut cache, deadline + 4).unwrap();
    assert_eq!(
        cache.full_passes,
        failed + 1,
        "failed passes must be retried"
    );
}

#[test]
fn unchanged_statistics_poll_needs_no_catalog_records() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let engine = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(SQLiteStorageBackend::new(connection.clone())),
    )
    .unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine.sql("CREATE TABLE items (id INTEGER); CREATE TABLE events (id INTEGER); ANALYZE items; ANALYZE events", &[]).unwrap();
    let mut cache = cache::MaintenanceCache::default();
    refresh_due_tables(&engine, &mut cache).unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.flush_prepared_statement_cache();
            sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "_uqa_mvcc_heads" | "_uqa_mvcc_versions" | "_uqa_mvcc_runs",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            Ok(())
        })
        .unwrap();
    for _ in 0..4 {
        refresh_due_tables(&engine, &mut cache).unwrap();
    }
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
    engine.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
    refresh_due_tables(&engine, &mut cache).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM items", &[])
            .unwrap()
            .rows[0]["n"],
        uqa_core::Value::Int(1)
    );
}
