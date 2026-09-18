//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role name and object reservations follow the session's transaction boundaries.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine, SQLResult,
};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};
use uqa_sql::SQLError;

fn after_wait(
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
    assert!(waited, "expected shared catalog wait on {target:?}");
    (worker, result.unwrap())
}

#[test]
fn concurrent_same_name_role_creators_wait_and_follow_commit_or_undo_for_every_provider() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_role; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "BEGIN; SAVEPOINT before_role; CREATE ROLE competing",
                );
                let original = first.durable.roles.read()["competing"].object_id;
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
                );
                let (second, result) = after_wait(
                    &first,
                    second,
                    "CREATE ROLE competing",
                    SharedCatalogLock::Name {
                        class_id: ROLE_CATALOG_CLASS_ID,
                        name: "competing",
                    },
                    finish,
                );
                let expected = if finish == "COMMIT" {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("23505"));
                    sql(&second, "ROLLBACK");
                    original
                } else {
                    result.unwrap();
                    let created = second.durable.roles.read()["competing"].object_id;
                    assert_ne!(created, original);
                    sql(&second, "COMMIT");
                    created
                };
                sql(&first, "SELECT rolname FROM pg_roles");
                assert_eq!(first.durable.roles.read()["competing"].object_id, expected);
                assert_eq!(
                    sql(&first, "SELECT v FROM t").rows.len(),
                    if finish == "COMMIT" { 1 } else { 2 }
                );
                drop(second);
                drop(first);
                let reopened =
                    super::identity::reopen(provider, &directory.path().join("table-locks.db"));
                assert_eq!(
                    reopened.durable.roles.read()["competing"].object_id,
                    expected
                );
            }
        }
    }
}
