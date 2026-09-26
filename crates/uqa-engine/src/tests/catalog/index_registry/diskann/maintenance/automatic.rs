//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{open, sql, Arc, Engine, Ordering, StorageReadControl};
use std::time::{Duration, Instant};
use uqa_execution::maintenance::diskann::DiskANNJournalMaintenance;
use uqa_storage::{
    diskann_index::{
        changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
        DiskANNIndexBinding,
    },
    PersistentStorageBackend, RelationIdentity,
};

fn stop(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

fn controlled_session(
    backend: &dyn PersistentStorageBackend,
    control: &StorageReadControl,
) -> uqa_storage::PersistentStorageSession {
    let session = backend.open_controlled_session(control).unwrap();
    assert!(session
        .backend
        .retention_control()
        .unwrap()
        .memory()
        .shares_allowance(control.memory()));
    assert!(session
        .backend
        .write_cancellation()
        .unwrap()
        .shares_signal(control.cancellation()));
    assert_ne!(
        session.backend.transaction_affinity(),
        backend.transaction_affinity()
    );
    session
}

fn pruner(
    backend: &dyn PersistentStorageBackend,
    control: &StorageReadControl,
) -> Box<dyn DiskANNJournalPruner> {
    backend
        .diskann_journal_pruner(
            DiskANNIndexBinding {
                table: "public.diskann_docs",
                field: "embedding",
                dimensions: 2,
                index: &RelationIdentity::new("public", "diskann_idx"),
                resolver: Arc::new(
                    uqa_execution::catalog::index::diskann::DiskANNIndexIdentityResolver,
                ),
                control,
            },
            8 << 20,
        )
        .unwrap()
}

fn page(
    backend: &dyn PersistentStorageBackend,
    pruner: &dyn DiskANNJournalPruner,
    request: DiskANNPruneRequest,
    control: &StorageReadControl,
) -> DiskANNPruneResult {
    backend.begin_transaction().unwrap();
    let result = pruner.prune(request, control).unwrap();
    backend.commit_transaction().unwrap();
    result
}

fn exercise(provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("automatic.db");
    let control = StorageReadControl::with_limit(8 << 20);
    let (engine, _repository) = open(&path, provider, &control);
    stop(&engine);
    sql(&engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    sql(&engine, "BEGIN");
    for _ in 0..70 {
        sql(
            &engine,
            "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]] WHERE id=1",
        );
    }
    sql(&engine, "COMMIT");
    let table = engine.try_table("diskann_docs").unwrap().unwrap();
    let retained = table
        .vector_indexes
        .read()
        .get("embedding")
        .unwrap()
        .snapshot()
        .unwrap();
    let old = retained.search_knn(&[1.0, 0.0], 2).unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    let session = controlled_session(&**backend, &control);
    let captured = pruner(&*session.backend, &control);
    assert!(captured
        .prune(
            DiskANNPruneRequest {
                after: None,
                max_records: 64
            },
            &control
        )
        .is_err());

    // Another maintainer removes a key after discovery. This still consumes one discovery slot, without restarting or losing the rest of the pass.
    let peer = controlled_session(&**backend, &control);
    let peer_pruner = pruner(&*peer.backend, &control);
    let first = page(
        &*peer.backend,
        &*peer_pruner,
        DiskANNPruneRequest {
            after: None,
            max_records: 1,
        },
        &control,
    );
    assert_eq!((first.examined, first.removed), (1, 1));
    drop(peer_pruner);
    drop(peer);
    let first = page(
        &*session.backend,
        &*captured,
        DiskANNPruneRequest {
            after: None,
            max_records: usize::MAX,
        },
        &control,
    );
    assert_eq!(first.examined, 64);
    assert!(first.next.is_some());
    sql(&engine, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=1; INSERT INTO diskann_docs VALUES(3,ARRAY[ARRAY[1.0,0.0]])");
    let last = page(
        &*session.backend,
        &*captured,
        DiskANNPruneRequest {
            after: first.next,
            max_records: 64,
        },
        &control,
    );
    assert_eq!(
        last.examined, 8,
        "new commits cannot extend the captured pass"
    );
    assert!(last.next.is_none());
    assert_eq!(first.removed + last.removed + 1, 72);
    drop(captured);
    drop(session);
    assert_eq!(retained.search_knn(&[1.0, 0.0], 2).unwrap(), old);

    exercise_worker(&engine, &control);
    assert_eq!(retained.search_knn(&[1.0, 0.0], 2).unwrap(), old);
}

fn exercise_coalescing(engine: &Engine, control: &StorageReadControl) {
    let backend = engine.storage.backend.as_ref().unwrap();
    // A deterministic host step uses the same Execution owner as the background worker. Its current pass must preserve both uncovered records.
    let mut maintenance = DiskANNJournalMaintenance::new(control).unwrap();
    let version = backend.change_version().unwrap();
    for _ in 0..12 {
        maintenance
            .step(
                engine.durable.catalog_indexes.snapshot(),
                backend.change_version().unwrap(),
                engine,
                &**backend,
            )
            .unwrap();
    }
    let status = maintenance.status();
    assert_eq!(
        (status.completed_passes, status.examined, status.removed),
        (1, 2, 0)
    );
    assert!(!status.pending_completion);
    assert_eq!(
        backend.change_version().unwrap(),
        version,
        "unchanged pages do not publish empty guarded transactions"
    );
    sql(
        engine,
        "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=3",
    );
    maintenance
        .step(
            engine.durable.catalog_indexes.snapshot(),
            backend.change_version().unwrap(),
            engine,
            &**backend,
        )
        .unwrap();
    sql(
        engine,
        "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=1",
    );
    for _ in 0..12 {
        maintenance
            .step(
                engine.durable.catalog_indexes.snapshot(),
                backend.change_version().unwrap(),
                engine,
                &**backend,
            )
            .unwrap();
    }
    let status = maintenance.status();
    assert_eq!(
        (status.completed_passes, status.examined, status.removed),
        (3, 7, 2),
        "a commit during a finite pass must trigger another pass"
    );
    drop(maintenance);
}

fn exercise_worker(engine: &Engine, control: &StorageReadControl) {
    exercise_coalescing(engine, control);
    let backend = engine.storage.backend.as_ref().unwrap();
    // Exercise the actual database-owned worker, including release/join and a retained pre-maintenance reader.
    sql(
        engine,
        "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0]] WHERE id=1",
    );
    engine
        .session
        .statistics_worker
        .store(false, Ordering::Release);
    engine.start_automatic_statistics();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.automatic_diskann_maintenance_status();
        assert!(status.last_error.is_none(), "{status:?}");
        if status.completed_passes > 0 {
            assert_eq!(status.removed, 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background maintenance did not complete: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stop(engine);
    let session = controlled_session(&**backend, control);
    let final_pruner = pruner(&*session.backend, control);
    let remaining = page(
        &*session.backend,
        &*final_pruner,
        DiskANNPruneRequest {
            after: None,
            max_records: 64,
        },
        control,
    );
    assert_eq!((remaining.examined, remaining.removed), (2, 0));
}

#[test]
fn diskann_automatic_journal_maintenance_native_sqlite() {
    exercise(0);
}

#[test]
fn diskann_automatic_journal_maintenance_sqlite_key_value() {
    exercise(1);
}

#[test]
fn diskann_automatic_journal_maintenance_redb() {
    exercise(2);
}
