//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence consumers complete a durable WAL prefix without synchronizing each autonomous log.

use super::*;
use uqa_storage::{
    PersistentStorageBackend, PersistentStorageSession, SequenceLogResult, SequenceValuePosition,
};

fn row() -> uqa_storage::SequenceRow {
    uqa_storage::SequenceRow {
        relation: uqa_storage::RelationIdentity::new("public", "sequence"),
        security: uqa_storage::SequenceSecurityRow::Bound(
            uqa_core::catalog_sequence::BoundSequenceSecurity::owner(
                uqa_core::catalog_role::RoleIdentity {
                    oid: 20_001,
                    object_id: [9; 16],
                },
            ),
        ),
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: uqa_storage::SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            cache_size: 1,
            ..uqa_storage::SequenceOptions::default()
        },
    }
}

fn initialize(connection: &ManagedConnection) -> (crate::SQLiteStorageBackend, SQLiteRecordStore) {
    crate::Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let catalog = crate::Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("public").unwrap();
    catalog.create_sequence_row(&row()).unwrap();
    let store = SQLiteRecordStore::for_native(connection, &control()).unwrap();
    (crate::SQLiteStorageBackend::new(connection.clone()), store)
}

fn log(session: &PersistentStorageSession, number: i64) {
    let expected = if number == 1 {
        (1, false)
    } else {
        ((number - 1) * 33, true)
    };
    assert_eq!(
        session
            .catalog
            .log_sequence_values(
                "public.sequence",
                [1; 16],
                [2; 16],
                expected,
                SequenceValuePosition {
                    current: number * 33,
                    called: true,
                    log_count: 0
                },
            )
            .unwrap(),
        SequenceLogResult::Logged
    );
}

fn boundary(store: &SQLiteRecordStore) -> (CommitSequence, CommitSequence) {
    store
        .with(|connection| {
            let header = codec::header(connection, store.identity)?;
            Ok((header.sequence, header.sequence_durable))
        })
        .unwrap()
}

fn commits(store: &SQLiteRecordStore) -> Arc<parking_lot::Mutex<Vec<bool>>> {
    let recorded = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let target = Arc::clone(&recorded);
    store
        .with(|connection| {
            let functions =
                super::super::connection_functions::ConnectionFunctions::of(connection)?;
            connection.commit_hook(Some(move || {
                target.lock().push(functions.synchronization_is_relaxed());
                false
            }))?;
            Ok(())
        })
        .unwrap();
    recorded
}

#[test]
fn native_sequence_logs_share_the_consumers_durable_commit_in_every_file_mode() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sequence.db");
        {
            let connection = super::reclamation::open(&path, mode);
            let (backend, store) = initialize(&connection);
            let wal = store
                .with(|connection| {
                    Ok(connection
                        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?)
                })
                .unwrap()
                .eq_ignore_ascii_case("wal");
            let recorded = commits(&store);
            backend.begin_read_transaction().unwrap();
            for number in 1..=48 {
                if number == 33 {
                    recorded.lock().clear();
                }
                let independent = backend
                    .open_sequence_value_session(&uqa_core::CancellationToken::new())
                    .unwrap();
                log(&independent, number);
            }
            let before = recorded.lock().clone();
            if wal {
                assert_eq!(
                    before.iter().filter(|relaxed| **relaxed).count(),
                    16,
                    "mode {mode}"
                );
                assert!(
                    before.iter().filter(|relaxed| !**relaxed).count() <= 1,
                    "{before:?}"
                );
                assert!(boundary(&store).1 < boundary(&store).0);
            } else {
                assert!(before.iter().all(|relaxed| !relaxed));
            }
            backend.commit_transaction().unwrap();
            let after = recorded.lock().clone();
            assert_eq!(after.len(), before.len() + usize::from(wal));
            assert_eq!(boundary(&store).0, boundary(&store).1);
            assert!(after.last().is_some_and(|relaxed| !relaxed));
            // The certificate prevents a second writer for a later cached-value consumer.
            backend.begin_read_transaction().unwrap();
            backend.require_sequence_value_durability().unwrap();
            backend.commit_transaction().unwrap();
            assert_eq!(*recorded.lock(), after);
        }
        let reopened = super::reclamation::open(&path, mode);
        reopened
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        let stored = crate::Catalog::open(reopened)
            .unwrap()
            .load_sequence_rows()
            .unwrap();
        assert_eq!(
            (stored[0].current, stored[0].called, stored[0].log_count),
            (48 * 33, true, 0)
        );
    }
}

#[test]
fn independent_cached_consumers_synchronize_another_sessions_publication() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequence.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let (producer, store) = initialize(&connection);
    producer.begin_read_transaction().unwrap();
    log(
        &producer
            .open_sequence_value_session(&uqa_core::CancellationToken::new())
            .unwrap(),
        1,
    );
    let published = boundary(&store);
    assert!(published.1 < published.0);
    let peer = ManagedConnection::open(&path).unwrap();
    peer.bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let consumer = crate::SQLiteStorageBackend::new(peer);
    consumer.begin_read_transaction().unwrap();
    consumer.require_sequence_value_durability().unwrap();
    consumer.commit_transaction().unwrap();
    assert_eq!(boundary(&store), (published.0, published.0));
    producer.rollback_transaction().unwrap();
    assert_eq!(boundary(&store), (published.0, published.0));
}

#[test]
fn savepoint_undo_and_cancelled_completion_keep_the_sequence_durability_obligation() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequence.db")).unwrap();
    let (backend, store) = initialize(&connection);
    backend.begin_read_transaction().unwrap();
    connection.savepoint("before").unwrap();
    log(
        &backend
            .open_sequence_value_session(&uqa_core::CancellationToken::new())
            .unwrap(),
        1,
    );
    connection.rollback_to_savepoint("before").unwrap();
    connection.release_savepoint("before").unwrap();
    let published = boundary(&store);
    assert!(published.1 < published.0);
    backend.write_cancellation().unwrap().cancel();
    assert!(backend.commit_transaction().is_err());
    assert!(backend.in_transaction());
    assert_eq!(boundary(&store), published);
    backend.rollback_transaction().unwrap();
    assert_eq!(boundary(&store), (published.0, published.0));
    assert!(!backend.in_transaction());
}

#[test]
fn a_failed_sequence_barrier_retains_completion_for_exact_retry() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequence.db")).unwrap();
    let (backend, store) = initialize(&connection);
    backend.begin_read_transaction().unwrap();
    log(
        &backend
            .open_sequence_value_session(&uqa_core::CancellationToken::new())
            .unwrap(),
        1,
    );
    let published = boundary(&store);
    store
        .with(|connection| {
            let mut fail = true;
            connection.commit_hook(Some(move || std::mem::take(&mut fail)))?;
            Ok(())
        })
        .unwrap();
    assert!(backend.commit_transaction().is_err());
    assert!(backend.in_transaction());
    assert_eq!(boundary(&store), published);
    backend.commit_transaction().unwrap();
    assert_eq!(boundary(&store), (published.0, published.0));
    assert!(!backend.in_transaction());
}

#[test]
fn ordinary_publication_covers_sequences_and_rollbacks_do_not_rewind_them() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequence.db")).unwrap();
    let (backend, store) = initialize(&connection);
    backend.begin_transaction().unwrap();
    let catalog = crate::Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("committed").unwrap();
    log(
        &backend
            .open_sequence_value_session(&uqa_core::CancellationToken::new())
            .unwrap(),
        1,
    );
    let recorded = commits(&store);
    backend.commit_transaction().unwrap();
    // One ordinary publication, plus at most one allocation reservation; no extra sequence barrier.
    let completed = recorded.lock().clone();
    assert!((1..=2).contains(&completed.len()), "{completed:?}");
    assert!(completed.iter().all(|relaxed| !relaxed));
    assert_eq!(boundary(&store).0, boundary(&store).1);
    backend.begin_transaction().unwrap();
    catalog.save_schema("discarded").unwrap();
    log(
        &backend
            .open_sequence_value_session(&uqa_core::CancellationToken::new())
            .unwrap(),
        2,
    );
    backend.rollback_transaction().unwrap();
    assert_eq!(boundary(&store).0, boundary(&store).1);
    let rows = crate::Catalog::open(connection.new_session())
        .unwrap()
        .load_sequence_rows()
        .unwrap();
    assert_eq!(rows[0].current, 66);
}

#[test]
fn synchronous_sessions_and_non_sequence_records_keep_full_publication() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequence.db")).unwrap();
    let (backend, store) = initialize(&connection);
    let recorded = commits(&store);
    log(&backend.open_session().unwrap(), 1);
    assert!(recorded.lock().iter().all(|relaxed| !relaxed));
    let independent = backend
        .open_sequence_value_session(&uqa_core::CancellationToken::new())
        .unwrap();
    recorded.lock().clear();
    independent.catalog.save_schema("ordinary").unwrap();
    assert!(recorded.lock().iter().all(|relaxed| !relaxed));
    assert_eq!(boundary(&store).0, boundary(&store).1);
}

#[test]
fn retained_sequence_publishers_are_synchronous_after_their_consumer_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequence.db")).unwrap();
    let (backend, store) = initialize(&connection);
    backend.begin_read_transaction().unwrap();
    let retained = backend
        .open_sequence_value_session(&uqa_core::CancellationToken::new())
        .unwrap();
    log(&retained, 1);
    backend.commit_transaction().unwrap();
    let recorded = commits(&store);
    log(&retained, 2);
    assert!(recorded.lock().iter().all(|relaxed| !relaxed));
    assert_eq!(boundary(&store).0, boundary(&store).1);
    backend.begin_read_transaction().unwrap();
    let current = backend
        .open_sequence_value_session(&uqa_core::CancellationToken::new())
        .unwrap();
    recorded.lock().clear();
    log(&retained, 3);
    assert!(recorded.lock().iter().all(|relaxed| !relaxed));
    log(&current, 4);
    assert!(recorded.lock().last().is_some_and(|relaxed| *relaxed));
    backend.commit_transaction().unwrap();
    assert_eq!(boundary(&store).0, boundary(&store).1);
}
