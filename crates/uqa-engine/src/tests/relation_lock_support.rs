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
use uqa_execution::row_locks::{shared_objects::SharedCatalogLock, RowLockKey};

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
    wait_for_relation_key(first, session, key, finished)
}

fn wait_for_relation_key(
    first: &Engine,
    session: u64,
    key: u64,
    finished: impl Fn() -> bool,
) -> bool {
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
    let key = holder.row_locks.table_key(relation);
    after_operation_key_wait(holder, worker, key, relation, release, operation)
}

pub(super) fn after_index_wait(
    holder: &Engine,
    worker: Engine,
    statement: &str,
    index: [u8; 16],
    release: &str,
) -> (Engine, Result<SQLResult, SQLError>) {
    let key = holder.row_locks.index_key(index);
    let statement = statement.to_owned();
    after_operation_key_wait(
        holder,
        worker,
        key,
        "index incarnation",
        release,
        move |worker| worker.sql(&statement, &[]),
    )
}

fn after_operation_key_wait<T: Send + 'static>(
    holder: &Engine,
    worker: Engine,
    key: u64,
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
    let waited = wait_for_relation_key(holder, session, key, || task.is_finished());
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
    assert!(
        waited,
        "expected a logical wait on {relation}; operation error: {:?}",
        result.as_ref().map(|result| result.as_ref().err())
    );
    (worker, result.unwrap())
}

pub(super) fn after_tuple_wait(
    holder: &Engine,
    worker: Engine,
    statement: &str,
    catalog: &str,
    doc_id: u64,
    release: &str,
) -> (Engine, Result<SQLResult, SQLError>) {
    after_tuple_wait_with_release(holder, worker, statement, catalog, doc_id, || {
        holder.sql(release, &[])
    })
}

pub(super) fn after_tuple_wait_with_release(
    holder: &Engine,
    worker: Engine,
    statement: &str,
    catalog: &str,
    doc_id: u64,
    release: impl FnOnce() -> Result<SQLResult, SQLError>,
) -> (Engine, Result<SQLResult, SQLError>) {
    let session = worker.session_id;
    let key = RowLockKey {
        table: holder.row_locks.table_key(catalog),
        doc_id,
    };
    let cancel = worker.runtime.cancellation.clone();
    let statement = statement.to_string();
    let (send, done) = mpsc::channel();
    let task = thread::spawn(move || {
        let result = worker.sql(&statement, &[]);
        let _ = send.send(result);
        worker
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while !holder.row_locks.waiting_for_row(session, key)
        && !task.is_finished()
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    let waited = holder.row_locks.waiting_for_row(session, key);
    let released = release();
    if released.is_err() {
        cancel.cancel();
    }
    let result = done.recv_timeout(Duration::from_secs(30));
    if result.is_err() {
        cancel.cancel();
    }
    let worker = task.join().unwrap();
    assert!(
        waited,
        "expected catalog tuple wait: {catalog}/{doc_id}; received {result:?}"
    );
    released.unwrap();
    (worker, result.unwrap())
}

pub(super) fn after_shared_wait(
    holder: &Engine,
    worker: Engine,
    statement: &str,
    target: SharedCatalogLock<'_>,
    release: &str,
) -> (Engine, Result<SQLResult, SQLError>) {
    let key = holder.row_locks.shared_catalog_key(target);
    let session = worker.session_id;
    let cancel = worker.runtime.cancellation.clone();
    let statement = statement.to_string();
    let (send, done) = mpsc::channel();
    let task = thread::spawn(move || {
        let result = worker.sql(&statement, &[]);
        let _ = send.send(result);
        worker
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while !holder.row_locks.waiting_for_relation(session, key)
        && !task.is_finished()
        && Instant::now() < deadline
    {
        thread::yield_now();
    }
    let waited = holder.row_locks.waiting_for_relation(session, key);
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
    assert!(
        waited,
        "expected shared catalog wait on {target:?}, received {result:?}"
    );
    (worker, result.unwrap())
}

pub(super) fn reopen(provider: usize, path: &std::path::Path) -> Engine {
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
