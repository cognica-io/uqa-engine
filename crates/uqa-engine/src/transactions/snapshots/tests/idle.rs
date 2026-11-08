//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use std::time::Duration;

#[test]
fn idle_memory_capture_retires_sources_without_retaining_undo_or_temporary_payloads() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'original'); CREATE SEQUENCE saved START 10 CACHE 4; SELECT nextval('saved'); CREATE TEMP TABLE temporary_data(body text); INSERT INTO temporary_data VALUES(repeat('t',1048576)); CREATE VIEW saved_view AS SELECT body FROM docs; CREATE TEMP VIEW temporary_view AS SELECT repeat('v',1048576)");
    let table = engine.table("docs").unwrap().unwrap();
    let original_id = table.object_id();
    let weak = Arc::downgrade(&table);
    drop(table);
    let views = engine.durable.views.snapshot();
    let weak_views = views.identity();
    drop(views);
    let control = StorageReadControl::with_limit(128 << 10);
    assert_eq!(engine.transaction_depth(), 0);
    let selected = capture(&engine, &control);
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        selected.names().unwrap(),
        [RelationIdentity::new("public", "docs")]
    );
    sql(&engine, "UPDATE docs SET body='later'; SELECT setval('saved',500); DROP VIEW saved_view; DROP TABLE docs; CREATE TABLE docs(replacement boolean)");
    drop(engine);
    assert!(weak.upgrade().is_none());
    assert!(!weak_views.is_alive());
    let table = selected
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    assert_eq!(table.object_id, original_id);
    assert_eq!(
        table.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("original".into()))
    );
    let sequences = selected.sequences.captured(&selected.cut).unwrap();
    assert_eq!(
        sequences
            .get()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .state
            .current,
        13
    );
    let views = selected.catalogs.views.captured(&selected.cut).unwrap();
    assert_eq!(views.get().unwrap().len(), 1);
    assert_eq!(
        views.get().unwrap().iter().next().unwrap().0,
        &RelationIdentity::new("public", "saved_view")
    );
    control.cancellation().cancel();
    assert!(table.document_store.len().is_err());
    assert!(views.get().is_err());
    drop((table, views, sequences, selected));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn idle_memory_capture_failures_leave_native_transaction_state_unchanged() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE docs(body text); INSERT INTO docs VALUES(repeat('x',32768))",
    );
    let denied = StorageReadControl::with_limit(1);
    assert!(engine.capture_committed_memory(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    assert_eq!(engine.transaction_depth(), 0);
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(engine.capture_committed_memory(&cancelled).is_err());
    assert_eq!(cancelled.memory().used(), 0);
    sql(&engine, "BEGIN; UPDATE docs SET body='private'; ROLLBACK");
    let funded = StorageReadControl::with_limit(1 << 20);
    let saved = capture(&engine, &funded);
    assert_eq!(
        saved
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "body")
            .unwrap(),
        Some(Value::Str("x".repeat(32768)))
    );
    assert_eq!(engine.transaction_depth(), 0);
    drop(saved);
    assert_eq!(funded.memory().used(), 0);
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn memory_capture_cancellation_finishes_before_held_admission_is_released(
    #[case] transaction_admission: bool,
) {
    let engine = Arc::new(Engine::new());
    let transactions = transaction_admission.then(|| engine.session.transactions.lock());
    let statement = (!transaction_admission).then(|| engine.lock_statement_gate());
    let control = StorageReadControl::with_limit(1 << 20);
    let worker_control = control.clone();
    let worker_engine = engine.clone();
    let (waited, blocked) = mpsc::sync_channel(1);
    let (finished, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        super::super::capture::observe_admission_wait(waited);
        finished
            .send(
                worker_engine
                    .capture_committed_memory(&worker_control)
                    .map(drop),
            )
            .unwrap();
    });
    blocked.recv_timeout(Duration::from_secs(30)).unwrap();
    control.cancellation().cancel();
    assert!(result
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .is_err());
    drop((transactions, statement));
    worker.join().unwrap();
    assert_eq!(control.memory().used(), 0);
    assert_eq!(engine.transaction_depth(), 0);
}

#[test]
fn waiting_memory_capture_uses_a_new_frame_before_its_statement_finishes() {
    let engine = Arc::new(Engine::new());
    sql(
        &engine,
        "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original')",
    );
    let statement = engine.lock_statement_gate();
    let control = StorageReadControl::with_limit(1 << 20);
    let worker_control = control.clone();
    let reader = engine.clone();
    let (waited, blocked) = mpsc::sync_channel(1);
    let (finished, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        super::super::capture::observe_admission_wait(waited);
        finished
            .send(reader.capture_committed_memory(&worker_control))
            .unwrap();
    });
    blocked.recv_timeout(Duration::from_secs(30)).unwrap();
    sql(&engine, "BEGIN; UPDATE docs SET body='private'; SAVEPOINT nested; ALTER TABLE docs ADD COLUMN extra integer");
    let selected = result
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let table = selected
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    assert_eq!(table.columns.get().unwrap().len(), 1);
    assert_eq!(
        table.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("original".into()))
    );
    sql(&engine, "ROLLBACK");
    drop(statement);
    worker.join().unwrap();
    drop((selected, table, engine));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_memory_frames_keep_the_original_capture_without_consuming_recovery() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original')",
    );
    sql(&engine, "BEGIN; UPDATE docs SET body='private'");
    assert!(engine.sql("SELECT 1/0", &[]).is_err());
    assert!(engine.transaction_failed());
    let control = StorageReadControl::with_limit(1 << 20);
    let selected = capture(&engine, &control);
    assert!(engine.transaction_failed());
    assert_eq!(engine.transaction_depth(), 1);
    sql(&engine, "ROLLBACK");
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "body")
            .unwrap(),
        Some(Value::Str("original".into()))
    );
    drop(selected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_capture_rejects_reentrant_unframed_writes_but_accepts_original_frames() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original')",
    );
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let _statement = engine.lock_statement_gate();
        assert!(engine.capture_committed_memory(&control).is_err());
        assert!(engine
            .capture_committed_memory_query(&control, |_| Ok(()))
            .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    engine
        .transaction(|engine| {
            sql(engine, "UPDATE docs SET body='private'");
            let selected = engine
                .capture_committed_memory_query(&control, |_| Ok(()))
                .unwrap();
            assert_eq!(
                selected
                    .table(&RelationIdentity::new("public", "docs"))
                    .unwrap()
                    .document_store
                    .get_field(1, "body")
                    .unwrap(),
                Some(Value::Str("original".into()))
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(control.memory().used(), 0);
    let selected = engine
        .capture_committed_memory_query(&control, |_| Ok(()))
        .unwrap();
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "body")
            .unwrap(),
        Some(Value::Str("private".into()))
    );
    drop(selected);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn memory_capture_races_transaction_completion_without_mixing_rows_and_catalogs(
    #[case] completion: &str,
) {
    let engine = Arc::new(Engine::new());
    sql(
        &engine,
        "CREATE TABLE docs(body text); INSERT INTO docs VALUES('original')",
    );
    for _ in 0..8 {
        sql(&engine, "BEGIN; UPDATE docs SET body='changed'; ALTER TABLE docs ADD COLUMN extra integer; SAVEPOINT nested; UPDATE docs SET body='nested'; ROLLBACK TO nested");
        let ready = Arc::new(std::sync::Barrier::new(2));
        let writer_ready = ready.clone();
        let writer = engine.clone();
        let completion = completion.to_owned();
        let worker = std::thread::spawn(move || {
            writer_ready.wait();
            sql(&writer, &completion);
        });
        let control = StorageReadControl::with_limit(1 << 20);
        ready.wait();
        let selected = capture(&engine, &control);
        worker.join().unwrap();
        let table = selected
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap();
        let columns = table.columns.get().unwrap().len();
        assert!(columns == 1 || columns == 2);
        assert_eq!(
            table.document_store.get_field(1, "body").unwrap(),
            Some(Value::Str(
                if columns == 1 { "original" } else { "changed" }.into()
            ))
        );
        drop((table, selected));
        assert_eq!(control.memory().used(), 0);
        sql(
            &engine,
            "ALTER TABLE docs DROP COLUMN IF EXISTS extra; UPDATE docs SET body='original'",
        );
    }
}
