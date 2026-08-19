//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `FOR UPDATE` / `FOR SHARE` execution coverage.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::{Engine, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::{SQLError, SQLResult};

#[derive(Default)]
struct RowLockSum(i64);

impl SQLAggregateState for RowLockSum {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        if let [Value::Int(value)] = args {
            self.0 += value;
        }
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Int(self.0))
    }
}

pub fn seed_accounts(engine: &Engine) {
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

pub fn sqlstate(error: &SQLError) -> String {
    error.sqlstate().unwrap_or("XX000").to_string()
}

fn wait_for_calls(calls: &AtomicUsize, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while calls.load(Ordering::SeqCst) < expected {
        assert!(
            Instant::now() < deadline,
            "volatile projection did not reach {expected} calls"
        );
        std::thread::yield_now();
    }
}

fn receive_after_unblock<T>(
    early: Result<T, mpsc::RecvTimeoutError>,
    receiver: &mpsc::Receiver<T>,
    timeout: Duration,
) -> (bool, T) {
    match early {
        Ok(value) => (false, value),
        Err(mpsc::RecvTimeoutError::Timeout) => (
            true,
            receiver
                .recv_timeout(timeout)
                .expect("waiter did not finish after unblock"),
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("waiter disconnected before returning a result")
        }
    }
}

fn assert_query_locks_only_one_row(root: &Engine, sql: &str) {
    let holder = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    let result = holder.sql(sql, &[]).unwrap();
    assert_eq!(result.rows.len(), 1, "{sql}");
    let locked = match result.rows[0].get("id") {
        Some(Value::Int(id)) => *id,
        other => panic!("expected integer id, got {other:?}"),
    };
    for id in 1..=3 {
        let result = probe.sql(
            &format!("SELECT id FROM accounts WHERE id = {id} FOR UPDATE NOWAIT"),
            &[],
        );
        if id == locked {
            let error = result.expect_err("the returned row must remain locked");
            assert_eq!(error.sqlstate(), Some("55P03"), "{sql}");
        } else {
            result.expect("an unconsumed row must remain unlocked");
        }
    }
    holder.sql("ROLLBACK", &[]).unwrap();
}

fn run_volatile_lock_wait(
    root: &Engine,
    calls: &Arc<AtomicUsize>,
    holder_sql: &str,
    tiny_work_mem: bool,
) -> SQLResult {
    calls.store(0, Ordering::SeqCst);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    if tiny_work_mem {
        waiter.sql("SET work_mem TO '1B'", &[]).unwrap();
    }
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql(holder_sql, &[]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, balance, row_lock_retry_tick() AS tick FROM accounts ORDER BY id FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    wait_for_calls(calls, 3);
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    result
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
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("join-scope.db")).unwrap();
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
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    let rows = holder
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
    waiter
        .sql(
            "SELECT name FROM owners WHERE name = 'ann' FOR UPDATE NOWAIT",
            &[],
        )
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
    assert_eq!(sqlstate(&error), "0A000");
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
    let early = done_rx.recv_timeout(Duration::from_millis(150));
    holder.sql("COMMIT", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    waiting_thread.join().unwrap();
    assert!(was_blocked);
    outcome.unwrap();
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
fn deadlock_victim_aborts_and_releases_its_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("deadlock-victim.db")).unwrap();
    seed_accounts(&root);
    let first = root.new_session().unwrap();
    let victim = root.new_session().unwrap();
    first.sql("BEGIN", &[]).unwrap();
    victim.sql("BEGIN", &[]).unwrap();
    first
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    victim
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let (blocked_tx, blocked_rx) = mpsc::channel();
    std::thread::spawn(move || {
        blocked_tx.send(()).unwrap();
        let result = first.sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[]);
        done_tx.send((first, result)).unwrap();
    });
    blocked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    // The first waiter must have registered its wait before the victim closes the cycle; the detector aborts whichever request closes it, so give the spawned request time to block without assuming timing.
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());

    let victim_outcome = victim.sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[]);
    let (first, first_outcome) = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let victim_deadlocked = victim_outcome
        .as_ref()
        .err()
        .is_some_and(|error| error.sqlstate() == Some("40P01"));
    let first_deadlocked = first_outcome
        .as_ref()
        .err()
        .is_some_and(|error| error.sqlstate() == Some("40P01"));
    assert!(
        victim_deadlocked ^ first_deadlocked,
        "exactly one side reports 40P01: victim {victim_outcome:?}, first {first_outcome:?}"
    );
    let (aborted, survivor) = if victim_deadlocked {
        (&victim, &first)
    } else {
        (&first, &victim)
    };
    let failed = aborted.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(failed.sqlstate(), Some("25P02"));
    survivor.sql("ROLLBACK", &[]).unwrap();
    aborted.sql("ROLLBACK", &[]).unwrap();
    aborted.sql("SELECT 1", &[]).unwrap();
}

#[test]
fn syntax_error_aborts_the_transaction_and_releases_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("syntax-error-abort.db")).unwrap();
    seed_accounts(&root);
    let failed = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    failed.sql("BEGIN", &[]).unwrap();
    failed
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let syntax = failed.sql("SELEC id FROM accounts", &[]).unwrap_err();
    assert_eq!(syntax.sqlstate(), Some("42601"));
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    let aborted = failed.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(aborted.sqlstate(), Some("25P02"));
    failed.sql("ROLLBACK", &[]).unwrap();
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
    let early = done_rx.recv_timeout(Duration::from_millis(150));
    cancel.cancel();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    waiting_thread.join().unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    assert!(was_blocked);
    let error = outcome.unwrap_err();
    assert_eq!(sqlstate(&error), "57014");
}

#[test]
fn separately_opened_engines_share_database_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("separate-roots.db");
    let holder = Engine::open(&path).unwrap();
    seed_accounts(&holder);
    let waiter = Engine::open(&path).unwrap();
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
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn committed_nested_frame_lock_survives_later_nested_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("nested-marks.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();

    let error = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn savepoint_rollback_restores_the_preexisting_weaker_strength() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("savepoint-strength.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR KEY SHARE", &[])
        .unwrap();
    holder.sql("SAVEPOINT stronger", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let blocked = waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR NO KEY UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&blocked), "55P03");

    holder.sql("ROLLBACK TO SAVEPOINT stronger", &[]).unwrap();
    waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR NO KEY UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn truncate_waits_for_the_relation_lock_held_by_for_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("truncate-relation-lock.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let truncater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let truncate_thread = std::thread::spawn(move || {
        done_tx
            .send(truncater.sql("TRUNCATE accounts", &[]))
            .unwrap();
    });
    let early = done_rx.recv_timeout(Duration::from_millis(150));
    holder
        .sql(
            "UPDATE accounts SET balance = balance + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    truncate_thread.join().unwrap();
    assert!(was_blocked);
    outcome.unwrap();
    assert!(root
        .sql("SELECT id FROM accounts", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn waiting_update_does_not_take_the_sqlite_writer_before_the_row_lock() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("writer-order.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        done_tx
            .send(waiter.sql("UPDATE accounts SET balance = 200 WHERE id = 1", &[]))
            .unwrap();
    });
    started_rx.recv().unwrap();
    let early = done_rx.recv_timeout(Duration::from_millis(100));

    holder
        .sql("UPDATE accounts SET balance = 150 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    update_thread.join().unwrap();
    assert!(was_blocked);
    outcome.unwrap();
    let result = root
        .sql("SELECT balance FROM accounts WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows[0]["balance"], Value::Int(200));
}

#[test]
fn multirow_update_locks_every_target_before_becoming_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("writer-row-cycle.db")).unwrap();
    seed_accounts(&root);
    let writer = root.new_session().unwrap();
    let row_holder = root.new_session().unwrap();
    row_holder.sql("BEGIN", &[]).unwrap();
    row_holder
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let writer_thread = std::thread::spawn(move || {
        done_tx
            .send(writer.sql(
                "UPDATE accounts SET balance = balance + 1 WHERE id IN (1, 2)",
                &[],
            ))
            .unwrap();
    });
    let early = done_rx.recv_timeout(Duration::from_millis(150));
    row_holder
        .sql(
            "UPDATE accounts SET balance = balance + 10 WHERE id = 2",
            &[],
        )
        .unwrap();
    row_holder.sql("COMMIT", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    writer_thread.join().unwrap();
    assert!(was_blocked);
    outcome.unwrap();
}

#[test]
fn merge_rechecks_the_join_after_a_concurrent_target_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merge-row-recheck.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE account_delta (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, balance INTEGER NOT NULL)",
        &[],
    )
    .unwrap();
    root.sql(
        "INSERT INTO account_delta (id, owner, balance) VALUES (4, 'ann', 900)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let worker = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("UPDATE accounts SET owner = 'zed' WHERE id = 1", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let merge_thread = std::thread::spawn(move || {
        done_tx
            .send(worker.sql(
                "MERGE INTO accounts AS target USING account_delta AS delta ON target.owner = delta.owner WHEN MATCHED THEN UPDATE SET balance = delta.balance WHEN NOT MATCHED THEN INSERT (id, owner, balance) VALUES (delta.id, delta.owner, delta.balance)",
                &[],
            ))
            .unwrap();
    });
    let early = done_rx.recv_timeout(Duration::from_millis(150));
    holder.sql("COMMIT", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    merge_thread.join().unwrap();
    assert!(was_blocked);
    let result = outcome.unwrap();
    assert_eq!(result.affected_rows, 1);
    let rows = root
        .sql(
            "SELECT id, owner, balance FROM accounts WHERE id IN (1, 4) ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows[0].get("owner"), Some(&Value::Str("zed".into())));
    assert_eq!(rows.rows[0].get("balance"), Some(&Value::Int(100)));
    assert_eq!(rows.rows[1].get("owner"), Some(&Value::Str("ann".into())));
    assert_eq!(rows.rows[1].get("balance"), Some(&Value::Int(900)));
}

#[path = "sql_row_locks/advanced.rs"]
mod advanced;
