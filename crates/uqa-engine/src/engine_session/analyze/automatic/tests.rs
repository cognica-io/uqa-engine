//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics sampling and publication preserve concurrent writes and newer changes.

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
fn compressed_statistics_publication_releases_reader_before_waiting_for_writer() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open_compressed(
        &directory.path().join("statistics.db"),
        uqa_storage::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    writer
        .sql(
            "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1)",
            &[],
        )
        .unwrap();
    let worker = writer.new_session().unwrap();
    for engine in [&writer, &worker] {
        engine.release_automatic_statistics_client();
        engine
            .session
            .statistics_worker
            .store(true, Ordering::Release);
    }
    let backend = worker.storage.backend.as_ref().unwrap();
    backend.begin_read_transaction().unwrap();
    worker.refresh_pinned_transaction_snapshot().unwrap();
    let sampled = worker
        .collect_automatic_analysis("public.t")
        .unwrap()
        .unwrap();
    backend.rollback_transaction().unwrap();
    writer.sql("BEGIN; INSERT INTO t VALUES (2)", &[]).unwrap();

    let worker_id = worker.session_id;
    let waiting_thread = std::thread::spawn(move || {
        let result = worker.publish_automatic_analysis("public.t", sampled);
        (worker, result)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !writer.row_locks.waiting_for_backend_writer(worker_id) {
        assert!(std::time::Instant::now() < deadline, "worker did not wait");
        std::thread::yield_now();
    }
    // A maintenance publication starts its own deferred transaction after
    // sampling. That new reader must also end before the logical writer wait.
    let committed = writer.sql("COMMIT", &[]);
    let (worker, published) = waiting_thread.join().unwrap();
    committed.unwrap();
    assert!(
        !published.unwrap(),
        "stale statistics replaced the newer rows"
    );
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

#[test]
fn full_and_automatic_analysis_bound_wide_values_without_changing_rows() {
    use uqa_core::Value;
    use uqa_sql::SQLParam;

    for automatic in [false, true] {
        let (_directory, writer, worker) = sessions();
        writer
            .sql(
                "CREATE TABLE payloads (id INTEGER PRIMARY KEY, body TEXT)",
                &[],
            )
            .unwrap();
        let large = "synthetic payload ".repeat(10_000);
        writer.begin().unwrap();
        for id in 0..8 {
            let body = match id {
                0..=2 => Value::Str("common".into()),
                3 => Value::Null,
                _ => Value::Str(format!("{id}:{large}")),
            };
            writer
                .sql(
                    "INSERT INTO payloads VALUES ($1, $2)",
                    &[SQLParam::scalar(Value::Int(id)), SQLParam::scalar(body)],
                )
                .unwrap();
        }
        writer.commit().unwrap();
        if automatic {
            assert!(worker.run_automatic_analyze("public.payloads").unwrap());
        } else {
            worker.run_analyze(Some("payloads")).unwrap();
        }
        let stats = worker.column_stats("payloads").unwrap();
        let body = &stats["body"];
        assert_eq!(body.row_count, 8);
        assert_eq!(body.null_count, 1);
        assert_eq!(body.distinct_count, 5);
        assert_eq!(body.mcv_values, [Value::Str("common".into())]);
        assert_eq!(body.mcv_frequencies, [3.0 / 8.0]);
        assert!(body
            .histogram
            .iter()
            .all(crate::engine_statistics::value_size::accepts));
        assert!(serde_json::to_string(&body.histogram).unwrap().len() < 100);
        let persisted = worker
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_column_stats("public.payloads")
            .unwrap();
        let persisted = persisted
            .iter()
            .find(|row| row.column_name == "body")
            .unwrap();
        assert!(persisted.histogram_json.len() + persisted.mcv_values_json.len() < 100);
        assert_eq!(
            writer
                .sql("SELECT body FROM payloads WHERE id = 7", &[])
                .unwrap()
                .rows[0]["body"],
            Value::Str(format!("7:{large}"))
        );
        assert!(
            !worker.run_automatic_analyze("public.payloads").unwrap(),
            "clean bounded statistics must not be recollected every poll"
        );
    }
}

#[test]
fn legacy_wide_statistics_are_bounded_on_reopen_and_replaced_without_new_writes() {
    use uqa_storage::ColumnStatsInput;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-wide-statistics.db");
    {
        let writer = Engine::open(&path).unwrap();
        writer.release_automatic_statistics_client();
        writer
            .session
            .statistics_worker
            .store(true, Ordering::Release);
        writer
            .sql(
                "CREATE TABLE items (body TEXT); INSERT INTO items VALUES ('current')",
                &[],
            )
            .unwrap();
        let huge = serde_json::to_string(&"synthetic legacy statistics ".repeat(50_000)).unwrap();
        let histogram = format!("[{huge},{huge}]");
        let catalog = writer.storage.catalog.as_ref().unwrap();
        catalog
            .save_column_stats(ColumnStatsInput {
                table_name: "public.items",
                column_name: "body",
                distinct_count: 1,
                null_count: 0,
                min_value: Some(&huge),
                max_value: Some(&huge),
                row_count: 1,
                histogram_json: &histogram,
                mcv_values_json: &format!("[{huge}]"),
                mcv_frequencies_json: "[1.0]",
            })
            .unwrap();
        catalog
            .set_metadata(
                "uqa.statistics.maintenance.v1:public.items",
                r#"{"generation":1,"changes":0,"analyzed_rows":1}"#,
            )
            .unwrap();
    }
    let reader = Engine::open(&path).unwrap();
    reader.release_automatic_statistics_client();
    reader
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    let table = reader.require_table("items").unwrap();
    let original = table.column_stats.snapshot();
    let stats = &original["body"];
    assert_eq!(stats.row_count, 1);
    assert_eq!(stats.distinct_count, 1);
    assert!(stats.min_value.is_none() && stats.max_value.is_none());
    assert!(
        stats.histogram.is_empty()
            && stats.mcv_values.is_empty()
            && stats.mcv_frequencies.is_empty()
    );
    assert!(reader.run_automatic_analyze("public.items").unwrap());
    let refreshed = reader.column_stats("items").unwrap();
    assert_eq!(
        refreshed["body"].min_value,
        Some(uqa_core::Value::Str("current".into()))
    );
    assert!(!reader.run_automatic_analyze("public.items").unwrap());
    let saved = reader
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_column_stats("public.items")
        .unwrap();
    assert!(saved[0].histogram_json.len() < 100);
}
