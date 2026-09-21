//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::Ordering;

fn persistent_engine(provider: usize, path: &std::path::Path) -> Engine {
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

#[test]
fn internal_read_sessions_and_fixed_snapshots_do_not_register_maintenance_clients() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("internal-reads.db");
        let root = persistent_engine(provider, &path);
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let internal = root.new_internal_read_session().unwrap();
        assert!(internal.session.statistics_worker.load(Ordering::Acquire));
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            internal.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        let pinned = root.open_independent_pinned_read_snapshot().unwrap();
        assert!(pinned.session.statistics_worker.load(Ordering::Acquire));
        assert!(!pinned.session.statistics_client.load(Ordering::Acquire));
        drop(pinned);
        let retained = root.open_retained_pinned_read_snapshot().unwrap();
        assert!(retained.session.statistics_worker.load(Ordering::Acquire));
        assert!(!retained.session.statistics_client.load(Ordering::Acquire));
        drop(retained);
        drop(internal);
        let public = root.new_session().unwrap();
        assert!(!public.session.statistics_worker.load(Ordering::Acquire));
        assert!(public.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            public.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
    }
}

#[test]
fn fixed_read_attachment_keeps_the_source_snapshot_while_latest_readers_advance() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("fixed-attachment.db"));
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let backend = root.storage.backend.as_ref().unwrap().clone();
        backend.begin_read_transaction().unwrap();
        let original_version = backend.change_version().unwrap();
        let id = root.live_table_doc_ids("t").unwrap()[0];
        peer.sql("UPDATE t SET v = 2; CREATE TABLE later(v integer)", &[])
            .unwrap();
        let retained = root.open_retained_pinned_read_snapshot().unwrap();
        let latest = root.open_independent_pinned_read_snapshot().unwrap();
        assert_eq!(
            retained.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(1)
        );
        assert_eq!(
            latest.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(2)
        );
        assert!(retained.try_table("later").unwrap().is_none());
        assert!(latest.try_table("later").unwrap().is_some());
        assert_eq!(
            retained
                .storage
                .backend
                .as_ref()
                .unwrap()
                .change_version()
                .unwrap(),
            original_version
        );
        backend
            .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
            .unwrap();
        assert_ne!(backend.change_version().unwrap(), original_version);
        backend.rollback_transaction().unwrap();
        drop((root, backend, latest));
        peer.sql("UPDATE t SET v = 3", &[]).unwrap();
        assert_eq!(
            retained.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(1)
        );
        assert!(retained.try_table("later").unwrap().is_none());
    }
}

#[test]
fn internal_read_workers_progress_while_the_source_statement_owns_its_transaction() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("worker-reads.db"));
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let backend = root.storage.backend.as_ref().unwrap();
        backend.begin_read_transaction().unwrap();
        peer.sql("UPDATE t SET v = 2; CREATE TABLE later(v integer)", &[])
            .unwrap();

        for retained in [false, true] {
            std::thread::scope(|scope| {
                let statement = root.runtime.statement_gate.lock();
                let transactions = root.session.transactions.lock();
                let (sender, receiver) = mpsc::channel();
                let source = &root;
                let worker = scope.spawn(move || {
                    let reader = if retained {
                        source.new_internal_retained_read_session()
                    } else {
                        source.new_internal_read_session()
                    }
                    .unwrap();
                    let value = reader.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"].clone();
                    let has_later = reader.try_table("later").unwrap().is_some();
                    assert!(!reader.session.statistics_client.load(Ordering::Acquire));
                    sender.send((value, has_later)).unwrap();
                });
                let completed = receiver.recv_timeout(Duration::from_secs(10));
                // Release the parent locks before joining even on failure, so a regression reports an error instead of hanging the test executable.
                drop(transactions);
                drop(statement);
                worker.join().unwrap();
                let (value, has_later) =
                    completed.expect("internal reader waited for its parent statement");
                assert_eq!(value, Value::Int(if retained { 1 } else { 2 }));
                assert_eq!(has_later, !retained);
            });
        }
        backend.rollback_transaction().unwrap();
    }
}
