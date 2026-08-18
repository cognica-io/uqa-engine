//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `FOR UPDATE` / `FOR SHARE` execution coverage.

use std::sync::mpsc;
use std::time::Duration;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLError;

fn seed_accounts(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE accounts (
                id INTEGER PRIMARY KEY,
                owner TEXT NOT NULL,
                balance INTEGER NOT NULL
            )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO accounts (id, owner, balance) VALUES
                (1, 'ann', 100),
                (2, 'bob', 200),
                (3, 'cara', 300)",
            &[],
        )
        .unwrap();
}

fn ids(engine: &Engine, sql: &str) -> Vec<i64> {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| match row.get("id").unwrap() {
            Value::Int(value) => *value,
            other => panic!("non-int id: {other:?}"),
        })
        .collect()
}

fn sqlstate(error: &SQLError) -> String {
    error.sqlstate().unwrap_or("XX000").to_string()
}

#[test]
fn for_update_returns_locked_rows_and_leaves_values_visible() {
    let engine = Engine::new();
    seed_accounts(&engine);
    engine.sql("BEGIN", &[]).unwrap();
    let locked = engine
        .sql("SELECT * FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    assert_eq!(locked.columns, ["id", "owner", "balance"]);
    assert!(locked.rows[0]
        .keys()
        .all(|column| !column.contains('\0') && !column.contains("lock")));
    assert_eq!(
        ids(&engine, "SELECT id FROM accounts ORDER BY id FOR UPDATE"),
        [1, 2, 3]
    );
    engine
        .sql(
            "UPDATE accounts SET balance = balance + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        ids(&engine, "SELECT id FROM accounts WHERE balance = 101"),
        [1]
    );
}

#[test]
fn for_update_of_one_join_input_locks_only_that_relation() {
    let engine = Engine::new();
    seed_accounts(&engine);
    engine
        .sql(
            "CREATE TABLE owners (name TEXT PRIMARY KEY, active BOOLEAN NOT NULL)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO owners (name, active) VALUES ('ann', TRUE), ('bob', TRUE)",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    let rows = engine
        .sql(
            "SELECT a.id
             FROM accounts AS a
             JOIN owners AS o ON o.name = a.owner
             ORDER BY a.id
             FOR UPDATE OF a",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    engine.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_of_nullable_join_side_is_rejected() {
    let engine = Engine::new();
    seed_accounts(&engine);
    engine
        .sql(
            "CREATE TABLE extras (account_id INTEGER PRIMARY KEY, note TEXT)",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "SELECT a.id
             FROM accounts AS a
             LEFT JOIN extras AS e ON e.account_id = a.id
             FOR UPDATE OF e",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("nullable side"));
}

#[test]
fn for_update_nowait_conflicts_with_a_sibling_session() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("locks.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_skip_locked_returns_the_next_unlocked_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("skip.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let worker = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    assert_eq!(
        ids(
            &worker,
            "SELECT id FROM accounts ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED"
        ),
        [2]
    );
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_waits_until_the_holder_commits() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("wait.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = waiter.sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[]);
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
}

#[test]
fn for_key_share_allows_non_key_update_and_blocks_delete() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("keyshare.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR KEY SHARE", &[])
        .unwrap();
    assert_eq!(
        ids(
            &updater,
            "SELECT id FROM accounts WHERE id = 1 FOR NO KEY UPDATE NOWAIT"
        ),
        [1]
    );
    let error = updater
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder
        .sql(
            "UPDATE accounts SET balance = balance + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        ids(
            &root,
            "SELECT id FROM accounts WHERE id = 1 AND balance = 101"
        ),
        [1]
    );
}

#[test]
fn point_update_takes_a_row_lock_visible_to_share_nowait() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("point-update.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("UPDATE accounts SET balance = 101 WHERE id = 1", &[])
        .unwrap();
    let error = waiter
        .sql("SELECT id FROM accounts WHERE id = 1 FOR SHARE NOWAIT", &[])
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn update_from_takes_a_row_lock_visible_to_share_nowait() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-from.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE owners (name TEXT PRIMARY KEY, active BOOLEAN NOT NULL)",
        &[],
    )
    .unwrap();
    root.sql(
        "INSERT INTO owners (name, active) VALUES ('ann', TRUE)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "UPDATE accounts SET balance = 101
             FROM owners
             WHERE owners.name = accounts.owner AND accounts.id = 1",
            &[],
        )
        .unwrap();
    let error = waiter
        .sql("SELECT id FROM accounts WHERE id = 1 FOR SHARE NOWAIT", &[])
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_of_a_view_does_not_lock_other_join_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("view-of.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE owners (name TEXT PRIMARY KEY, active BOOLEAN NOT NULL)",
        &[],
    )
    .unwrap();
    root.sql(
        "INSERT INTO owners (name, active) VALUES ('ann', TRUE), ('bob', TRUE)",
        &[],
    )
    .unwrap();
    root.sql(
        "CREATE VIEW balances AS SELECT id, owner, balance FROM accounts",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT b.id
             FROM balances AS b
             JOIN owners AS o ON o.name = b.owner
             WHERE b.id = 1
             FOR UPDATE OF b",
            &[],
        )
        .unwrap();
    let owners = waiter
        .sql(
            "SELECT name FROM owners WHERE name = 'ann' FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    assert_eq!(owners.rows.len(), 1);
    let error = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_on_a_view_locks_the_underlying_table() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("view.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE VIEW balances AS SELECT id, balance FROM accounts",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM balances WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn savepoint_rollback_releases_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("savepoint.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("SAVEPOINT taken", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 3 FOR UPDATE", &[])
        .unwrap();
    let blocked = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&blocked), "55P03");
    holder.sql("ROLLBACK TO SAVEPOINT taken", &[]).unwrap();
    waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn for_update_cancel_during_wait_is_57014() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cancel.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    let cancel = waiter.cancellation_token();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = waiter.sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[]);
        done_tx.send(result).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    cancel.cancel();
    let error = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert_eq!(sqlstate(&error), "57014");
    waiting_thread.join().unwrap();
    holder.sql("COMMIT", &[]).unwrap();
}
