//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent sessions and observed logical waits for relation-lock schedules.

use super::*;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(super) fn sessions(provider: usize) -> (tempfile::TempDir, Engine, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("table-locks.db");
    let first = match provider {
        0 => Engine::open(&path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let second = first.new_session().unwrap();
    for engine in [&first, &second] {
        engine.release_automatic_statistics_client();
        engine
            .session
            .statistics_worker
            .store(true, Ordering::Release);
    }
    sql(
        &first,
        "CREATE TABLE t(v integer); INSERT INTO t VALUES (1)",
    );
    (directory, first, second)
}

pub(super) fn sql(engine: &Engine, statement: &str) -> SQLResult {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
}

pub(super) fn error(engine: &Engine, statement: &str, state: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{statement}: {error}");
}

pub(super) fn wait_for_relation(
    first: &Engine,
    session: u64,
    relation: &str,
    finished: impl Fn() -> bool,
) -> bool {
    let key = first.row_locks.table_key(relation);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !first.row_locks.waiting_for_relation(session, key)
        && !finished()
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    first.row_locks.waiting_for_relation(session, key)
}

pub(super) fn after_wait(
    holder: &Engine,
    worker: Engine,
    statement: &str,
    relation: &str,
    release: &str,
) -> (Engine, Result<SQLResult, SQLError>) {
    let statement = statement.to_string();
    after_operation_wait(holder, worker, relation, release, move |worker| {
        worker.sql(&statement, &[])
    })
}

pub(super) fn after_operation_wait<T: Send + 'static>(
    holder: &Engine,
    worker: Engine,
    relation: &str,
    release: &str,
    operation: impl FnOnce(&Engine) -> Result<T, SQLError> + Send + 'static,
) -> (Engine, Result<T, SQLError>) {
    let session = worker.session_id;
    let cancel = worker.runtime.cancellation.clone();
    let (send, done) = mpsc::channel();
    let task = thread::spawn(move || {
        let result = operation(&worker);
        let _ = send.send(result);
        worker
    });
    let waited = wait_for_relation(holder, session, relation, || task.is_finished());
    let released = holder.sql(release, &[]);
    if released.is_err() {
        cancel.cancel();
    }
    let result = done.recv_timeout(Duration::from_secs(30));
    if result.is_err() {
        cancel.cancel();
    }
    let worker = task.join().unwrap();
    released.unwrap();
    assert!(waited, "expected a logical wait on {relation}");
    (worker, result.unwrap())
}
