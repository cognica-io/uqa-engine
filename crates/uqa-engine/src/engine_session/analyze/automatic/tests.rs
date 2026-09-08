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

#[test]
fn statistics_commit_preserves_unpublished_document_id_reservations() {
    for promote in [false, true] {
        let (_directory, writer, worker) = sessions();
        writer
            .sql("CREATE TABLE entries (key TEXT PRIMARY KEY)", &[])
            .unwrap();
        writer.begin().unwrap();
        let first = writer.allocate_next_id("entries").unwrap();
        assert!(worker.run_automatic_analyze("public.t").unwrap());
        if promote {
            writer.prepare_explicit_transaction_writer().unwrap();
        } else {
            writer.refresh_explicit_statement_snapshot().unwrap();
        }
        let second = writer.allocate_next_id("entries").unwrap();
        assert_eq!(
            second,
            first + 1,
            "catalog refresh reused a reserved document ID (promote={promote})"
        );
        writer.rollback().unwrap();
    }
}

#[test]
fn copy_preserves_all_rows_when_statistics_commit_during_staging() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    use uqa_core::Value;
    use uqa_sql::SQLError;

    let (directory, writer, worker) = sessions();
    let worker = Arc::new(worker);
    let maintenance = Arc::downgrade(&worker);
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = Arc::clone(&calls);
    writer
        .register_scalar_function("maintain_statistics", move |_: &[Value]| {
            if invoked.fetch_add(1, Ordering::AcqRel) == 1 {
                assert!(maintenance
                    .upgrade()
                    .unwrap()
                    .run_automatic_analyze("public.t")
                    .unwrap());
            }
            Ok::<_, SQLError>(Value::Int(0))
        })
        .unwrap();
    writer
        .sql(
            "CREATE TABLE entries (key TEXT PRIMARY KEY, marker INTEGER DEFAULT maintain_statistics())",
            &[],
        )
        .unwrap();
    writer.begin().unwrap();
    for input in [b"first\nsecond\n".as_slice(), b"third\nfourth\n".as_slice()] {
        assert_eq!(
            writer
                .copy_from("COPY entries (key) FROM STDIN", input)
                .unwrap(),
            2
        );
    }
    writer.commit().unwrap();
    assert_eq!(calls.load(Ordering::Acquire), 4);
    let expected = ["first", "fourth", "second", "third"].map(|key| Value::Str(key.to_string()));
    let rows = writer
        .sql("SELECT key FROM entries ORDER BY key", &[])
        .unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["key"].clone())
            .collect::<Vec<_>>(),
        expected
    );
    // The scheduling hook is process-local; persist only ordinary SQL defaults
    // before testing the independent reopen boundary.
    writer
        .sql("ALTER TABLE entries ALTER COLUMN marker DROP DEFAULT", &[])
        .unwrap();
    drop(writer);
    drop(worker);
    let reopened = Engine::open(&directory.path().join("statistics.db")).unwrap();
    let rows = reopened
        .sql("SELECT key FROM entries ORDER BY key", &[])
        .unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["key"].clone())
            .collect::<Vec<_>>(),
        expected
    );
}
