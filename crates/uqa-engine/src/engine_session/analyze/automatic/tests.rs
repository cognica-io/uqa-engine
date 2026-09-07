//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics snapshots cannot block WAL writers or supersede newer changes.

use super::*;

fn sessions() -> (tempfile::TempDir, Engine, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("statistics.db")).unwrap();
    writer
        .sql(
            "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1)",
            &[],
        )
        .unwrap();
    let worker = writer.new_session().unwrap();
    // These deterministic tests drive the worker phases themselves; release
    // their automatic-client registrations without altering database state.
    worker.release_automatic_statistics_client();
    writer.release_automatic_statistics_client();
    worker
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    writer
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    (directory, writer, worker)
}

#[test]
fn sampling_read_snapshot_allows_writes_and_rejects_obsolete_statistics() {
    let (_directory, writer, worker) = sessions();
    let backend = worker.storage.backend.as_ref().unwrap();
    backend.begin_read_transaction().unwrap();
    worker.refresh_pinned_transaction_snapshot().unwrap();
    let sampled = worker
        .collect_automatic_analysis("public.t")
        .unwrap()
        .unwrap();
    assert_eq!(sampled.row_count, 1);
    // Must complete while the maintenance reader is pinned; sampling cannot
    // reserve the backend writer or the application's statement gate.
    writer.sql("INSERT INTO t VALUES (2)", &[]).unwrap();
    backend.rollback_transaction().unwrap();
    assert!(!worker
        .publish_automatic_analysis("public.t", sampled)
        .unwrap());
    assert!(worker.run_automatic_analyze("public.t").unwrap());
    assert_eq!(worker.column_stats("t").unwrap()["id"].row_count, 2);
}

#[test]
fn sampling_cannot_publish_into_a_same_name_replacement() {
    let (_directory, writer, worker) = sessions();
    let backend = worker.storage.backend.as_ref().unwrap();
    backend.begin_read_transaction().unwrap();
    worker.refresh_pinned_transaction_snapshot().unwrap();
    let sampled = worker
        .collect_automatic_analysis("public.t")
        .unwrap()
        .unwrap();
    backend.rollback_transaction().unwrap();
    writer.sql("DROP TABLE t; CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (3), (4), (5)", &[]).unwrap();
    assert!(!worker
        .publish_automatic_analysis("public.t", sampled)
        .unwrap());
    assert!(worker.run_automatic_analyze("public.t").unwrap());
    assert_eq!(worker.column_stats("t").unwrap()["id"].row_count, 3);
}

#[test]
fn explicit_analysis_supersedes_an_inflight_automatic_sample() {
    let (_directory, writer, worker) = sessions();
    let backend = worker.storage.backend.as_ref().unwrap();
    backend.begin_read_transaction().unwrap();
    worker.refresh_pinned_transaction_snapshot().unwrap();
    let sampled = worker
        .collect_automatic_analysis("public.t")
        .unwrap()
        .unwrap();
    backend.rollback_transaction().unwrap();
    writer.run_analyze(Some("t")).unwrap();
    assert!(!worker
        .publish_automatic_analysis("public.t", sampled)
        .unwrap());
}
