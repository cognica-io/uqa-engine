//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent sessions and observed logical waits for relation-lock schedules.

use super::*;
use std::{
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
