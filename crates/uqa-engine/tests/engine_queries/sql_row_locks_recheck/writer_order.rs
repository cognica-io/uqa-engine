//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[path = "writer_order/insert_select_progress.rs"]
mod insert_select_progress;
#[path = "writer_order/update_from_source.rs"]
mod update_from_source;

#[test]
fn writer_promoted_after_savepoint_keeps_its_deadlock_registration() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("savepoint-writer-lock.db")).unwrap();
    root.sql(
        "CREATE TABLE savepoint_writer (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO savepoint_writer VALUES (1, 0), (2, 0), (3, 0)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let blocker = root.new_session().unwrap();
    writer.sql("BEGIN", &[]).unwrap();
    writer.sql("SAVEPOINT promoted", &[]).unwrap();
    writer
        .sql("UPDATE savepoint_writer SET value = 1 WHERE id = 1", &[])
        .unwrap();
    writer.sql("ROLLBACK TO SAVEPOINT promoted", &[]).unwrap();
    blocker.sql("BEGIN", &[]).unwrap();
    blocker
        .sql(
            "SELECT id FROM savepoint_writer WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (writer_tx, writer_rx) = mpsc::channel();
    let writer_wait = std::thread::spawn(move || {
        let result = writer.sql(
            "SELECT id FROM savepoint_writer WHERE id = 2 FOR UPDATE",
            &[],
        );
        writer.sql("ROLLBACK", &[]).ok();
        writer_tx.send(result).unwrap();
    });
    let blocker_result = blocker.sql("UPDATE savepoint_writer SET value = 1 WHERE id = 3", &[]);
    blocker.sql("ROLLBACK", &[]).ok();
    let writer_result = writer_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        blocker_result
            .as_ref()
            .is_err_and(|error| sqlstate(error) == "40P01")
            || writer_result
                .as_ref()
                .is_err_and(|error| sqlstate(error) == "40P01"),
        "one participant must detect the writer/row-lock cycle; blocker: {blocker_result:?}, writer: {writer_result:?}"
    );
    writer_wait.join().unwrap();
}

#[test]
fn for_share_lock_rechecks_after_a_conflicting_non_key_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("for-share-recheck.db")).unwrap();
    seed_accounts(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "for_share_scan_gate",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            if observed_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).unwrap();
                callback_gate.wait();
            }
            Ok(Value::Int(1))
        },
    )
    .unwrap();
    let reader = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        done_tx
            .send(reader.sql(
                "SELECT id, balance, for_share_scan_gate() AS gate FROM accounts WHERE id = 1 ORDER BY gate FOR SHARE",
                &[],
            ))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    updater
        .sql("UPDATE accounts SET balance = 777 WHERE id = 1", &[])
        .unwrap();
    gate.wait();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    reader_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(777)));
}

#[test]
fn insert_select_for_update_lets_the_row_lock_holder_commit_first() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-select-writer-order.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE audit (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO audit SELECT id, balance FROM accounts WHERE id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 111 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    let audited = root
        .sql("SELECT balance FROM audit WHERE id = 1", &[])
        .unwrap();
    assert_eq!(audited.rows[0].get("balance"), Some(&Value::Int(111)));
}

#[test]
fn multi_table_truncate_locks_every_target_before_becoming_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("truncate-all-locks-first.db")).unwrap();
    root.sql(
        "CREATE TABLE truncate_a (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE truncate_b (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO truncate_a VALUES (1, 0); INSERT INTO truncate_b VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let truncater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM truncate_b WHERE id = 1 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let truncate_thread = std::thread::spawn(move || {
        done_tx
            .send(truncater.sql("TRUNCATE truncate_a, truncate_b", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE truncate_b SET value = 1 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    truncate_thread.join().unwrap();
    assert_eq!(
        root.sql(
            "SELECT (SELECT count(*) FROM truncate_a) + (SELECT count(*) FROM truncate_b) AS rows_left",
            &[],
        )
        .unwrap()
        .rows[0]["rows_left"],
        Value::Int(0)
    );
}

#[test]
fn mutating_locking_select_does_not_hold_the_data_writer_while_waiting() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("locking-nextval-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE locking_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE SEQUENCE locking_projection_seq START 1; INSERT INTO locking_source VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM locking_source WHERE id = 1 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT nextval('locking_projection_seq') AS sequence_value, id FROM locking_source WHERE id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE locking_source SET value = 1 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    // PostgreSQL evaluates the volatile target once for the original tuple and once for the EvalPlanQual replacement committed by the holder.
    assert_eq!(result.rows[0]["sequence_value"], Value::Int(2));
}

#[test]
fn insert_values_locks_scalar_subquery_before_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-values-lock-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE insert_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE insert_dest (id INTEGER PRIMARY KEY, value INTEGER); CREATE SEQUENCE insert_dest_seq START 1; INSERT INTO insert_source VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM insert_source WHERE id = 1 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO insert_dest VALUES (nextval('insert_dest_seq'), (SELECT value FROM insert_source WHERE id = 1 FOR UPDATE))",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE insert_source SET value = 7 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT value FROM insert_dest WHERE id = 1", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
}

#[test]
fn insert_returning_locking_subquery_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-returning-lock-order.db")).unwrap();
    root.sql(
        "CREATE TABLE returning_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE returning_target (id INTEGER PRIMARY KEY); INSERT INTO returning_source VALUES (1, 7)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM returning_source WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO returning_target VALUES (1) RETURNING (SELECT value FROM returning_source WHERE id = 1 FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE returning_source SET value = value + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.rows[0]["locked_value"], Value::Int(8));
}

#[test]
fn unreachable_returning_locking_subquery_does_not_lock_its_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("conditional-returning-lock.db")).unwrap();
    root.sql(
        "CREATE TABLE conditional_returning_source (id INTEGER PRIMARY KEY); CREATE TABLE conditional_returning_target (id INTEGER PRIMARY KEY); INSERT INTO conditional_returning_source VALUES (1)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conditional_returning_source WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO conditional_returning_target VALUES (1) RETURNING CASE WHEN false THEN (SELECT id FROM conditional_returning_source WHERE id = 1 FOR UPDATE) ELSE 1 END AS value",
                &[],
            ))
            .unwrap();
    });
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(1));
}

#[test]
fn correlated_insert_returning_rechecks_with_the_same_outer_row_before_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("correlated-insert-returning-lock-order.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE correlated_returning_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE correlated_returning_target (id INTEGER PRIMARY KEY); INSERT INTO correlated_returning_source VALUES (1, 10), (2, 20)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM correlated_returning_source WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO correlated_returning_target VALUES (1), (2) RETURNING id, (SELECT value FROM correlated_returning_source AS source WHERE source.id = correlated_returning_target.id FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE correlated_returning_source SET value = value + 1 WHERE id = 2",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["locked_value"], Value::Int(10));
    assert_eq!(result.rows[1]["locked_value"], Value::Int(21));
}

#[test]
fn insert_returning_rebuilds_after_a_concurrent_conflict_commits() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("insert-returning-concurrent-conflict.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE concurrent_conflict_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE concurrent_conflict_target (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO concurrent_conflict_source VALUES (1, 10), (101, 1010)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    writer.sql("BEGIN", &[]).unwrap();
    writer
        .sql(
            "INSERT INTO concurrent_conflict_target VALUES (1, 100)",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO concurrent_conflict_target VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET value = concurrent_conflict_target.value + 1 RETURNING value, (SELECT value FROM concurrent_conflict_source AS source WHERE source.id = concurrent_conflict_target.value FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    writer.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(101));
    assert_eq!(result.rows[0]["locked_value"], Value::Int(1010));
}

#[test]
fn on_conflict_do_nothing_releases_its_key_reservation_after_the_statement() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("do-nothing-key-reservation-lifetime.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE key_reservation_target (id INTEGER PRIMARY KEY); INSERT INTO key_reservation_target VALUES (1)",
        &[],
    )
    .unwrap();
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();
    first.sql("BEGIN", &[]).unwrap();
    let skipped = first
        .sql(
            "INSERT INTO key_reservation_target VALUES (1) ON CONFLICT DO NOTHING",
            &[],
        )
        .unwrap();
    assert_eq!(skipped.affected_rows, 0);

    let (done_tx, done_rx) = mpsc::channel();
    let second_thread = std::thread::spawn(move || {
        done_tx
            .send(second.sql(
                "INSERT INTO key_reservation_target VALUES (1) ON CONFLICT DO NOTHING",
                &[],
            ))
            .unwrap();
    });
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    second_thread.join().unwrap();
    first.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(result.affected_rows, 0);
}

#[test]
fn nested_uncorrelated_lock_recheck_does_not_inherit_a_correlated_outer_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("nested-uncorrelated-returning-lock.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE nested_returning_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE nested_returning_target (id INTEGER PRIMARY KEY); INSERT INTO nested_returning_source VALUES (1, 60)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM nested_returning_source WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO nested_returning_target VALUES (2) RETURNING (SELECT (SELECT value FROM nested_returning_source WHERE id = 1 FOR UPDATE) WHERE nested_returning_target.id = 2) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE nested_returning_source SET value = value + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.rows[0]["locked_value"], Value::Int(61));
}

#[test]
fn update_returning_locking_subquery_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-returning-lock-order.db")).unwrap();
    root.sql(
        "CREATE TABLE update_returning_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE update_returning_target (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO update_returning_source VALUES (1, 30); INSERT INTO update_returning_target VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM update_returning_source WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql(
                "UPDATE update_returning_target SET value = value + 1 WHERE id = 1 RETURNING id, (SELECT value FROM update_returning_source AS source WHERE source.id = update_returning_target.id FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE update_returning_source SET value = value + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.rows[0]["locked_value"], Value::Int(31));
}

fn register_snapshot_visibility_functions(root: &Arc<Engine>) {
    root.sql(
        "CREATE TABLE snapshot_target (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE snapshot_source (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO snapshot_source VALUES (1, 100), (2, 200), (3, 300)",
        &[],
    )
    .unwrap();
    let count_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "visible_snapshot_count",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            let engine = count_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("RETURNING snapshot engine was dropped".into())
            })?;
            Ok(Value::Int(
                i64::try_from(engine.table_doc_ids("snapshot_target")?.len()).unwrap(),
            ))
        },
    )
    .unwrap();
    let sum_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "visible_snapshot_sum",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            let engine = sum_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("RETURNING snapshot engine was dropped".into())
            })?;
            let mut sum = 0_i64;
            for doc_id in engine.table_doc_ids("snapshot_target")? {
                if let Some(Value::Int(value)) = engine
                    .get_document("snapshot_target", doc_id)?
                    .and_then(|document| document.get("value").cloned())
                {
                    sum += value;
                }
            }
            Ok(Value::Int(sum))
        },
    )
    .unwrap();
}

#[test]
fn dml_returning_scalar_subqueries_read_the_statement_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        Arc::new(Engine::open(&directory.path().join("returning-statement-snapshot.db")).unwrap());
    register_snapshot_visibility_functions(&root);

    let inserted = root
        .sql(
            "INSERT INTO snapshot_target VALUES (1, 10), (2, 20), (3, 30) RETURNING id, (SELECT count(*) FROM snapshot_target) AS snapshot_count, (SELECT visible_snapshot_count() WHERE snapshot_target.id > 0) AS visible_count",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.rows.len(), 3);
    assert!(inserted
        .rows
        .iter()
        .all(|row| row["snapshot_count"] == Value::Int(0)));
    assert_eq!(
        inserted
            .rows
            .iter()
            .map(|row| row["visible_count"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );

    let updated = root
        .sql(
            "UPDATE snapshot_target SET value = value + 1 RETURNING id, (SELECT sum(value) FROM snapshot_target) AS snapshot_sum, (SELECT visible_snapshot_sum() WHERE snapshot_target.id > 0) AS visible_sum",
            &[],
        )
        .unwrap();
    assert_eq!(updated.rows.len(), 3);
    assert!(updated
        .rows
        .iter()
        .all(|row| row["snapshot_sum"] == Value::Int(60)));
    assert_eq!(
        updated
            .rows
            .iter()
            .map(|row| row["visible_sum"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(61), Value::Int(62), Value::Int(63)]
    );

    root.sql("UPDATE snapshot_target SET value = value - 1", &[])
        .unwrap();
    let merged = root
        .sql(
            "MERGE INTO snapshot_target AS target USING snapshot_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value RETURNING target.id, (SELECT sum(value) FROM snapshot_target) AS snapshot_sum, (SELECT visible_snapshot_sum() WHERE target.id > 0) AS visible_sum",
            &[],
        )
        .unwrap();
    assert_eq!(merged.rows.len(), 3);
    assert!(merged
        .rows
        .iter()
        .all(|row| row["snapshot_sum"] == Value::Int(60)));
    assert_eq!(
        merged
            .rows
            .iter()
            .map(|row| row["visible_sum"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(150), Value::Int(330), Value::Int(600)]
    );

    let deleted = root
        .sql(
            "DELETE FROM snapshot_target RETURNING id, (SELECT count(*) FROM snapshot_target) AS snapshot_count, (SELECT visible_snapshot_count() WHERE snapshot_target.id > 0) AS visible_count",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.rows.len(), 3);
    assert!(deleted
        .rows
        .iter()
        .all(|row| row["snapshot_count"] == Value::Int(3)));
    assert_eq!(
        deleted
            .rows
            .iter()
            .map(|row| row["visible_count"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(2), Value::Int(1), Value::Int(0)]
    );
}

fn register_command_progress_functions(root: &Arc<Engine>) {
    let count_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "visible_command_count",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |args: &[Value]| {
            let engine = count_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("INSERT command-progress engine was dropped".into())
            })?;
            let Some(Value::Str(table)) = args.first() else {
                return Err(uqa_sql::SQLError::TypeMismatch(
                    "visible_command_count expects one table name".into(),
                ));
            };
            Ok(Value::Int(
                i64::try_from(engine.table_doc_ids(table)?.len()).unwrap(),
            ))
        },
    )
    .unwrap();
    let sum_engine = Arc::downgrade(root);
    root.register_scalar_function_with_options(
        "visible_command_sum",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |args: &[Value]| {
            let engine = sum_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("INSERT command-progress engine was dropped".into())
            })?;
            let Some(Value::Str(table)) = args.first() else {
                return Err(uqa_sql::SQLError::TypeMismatch(
                    "visible_command_sum expects one table name".into(),
                ));
            };
            let mut sum = 0_i64;
            for doc_id in engine.table_doc_ids(table)? {
                if let Some(Value::Int(value)) = engine
                    .get_document(table, doc_id)?
                    .and_then(|document| document.get("seen").cloned())
                {
                    sum += value;
                }
            }
            Ok(Value::Int(sum))
        },
    )
    .unwrap();
}

#[test]
fn dml_volatile_expressions_see_preceding_command_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        Arc::new(Engine::open(&directory.path().join("insert-command-progress.db")).unwrap());
    register_command_progress_functions(&root);
    root.sql(
        "CREATE TABLE progress_values (id INTEGER PRIMARY KEY, seen INTEGER, snapshot_count INTEGER); CREATE TABLE progress_defaults (id INTEGER PRIMARY KEY, seen INTEGER DEFAULT visible_command_count('progress_defaults')); CREATE TABLE progress_conflicts (id INTEGER PRIMARY KEY, seen INTEGER); CREATE TABLE progress_updates (id INTEGER PRIMARY KEY, seen INTEGER); INSERT INTO progress_conflicts VALUES (1, 10), (2, 20), (3, 30); INSERT INTO progress_updates VALUES (1, 10), (2, 20), (3, 30)",
        &[],
    )
    .unwrap();

    let values = root
        .sql(
            "INSERT INTO progress_values VALUES (1, visible_command_count('progress_values'), (SELECT count(*) FROM progress_values)), (2, visible_command_count('progress_values'), (SELECT count(*) FROM progress_values)), (3, visible_command_count('progress_values'), (SELECT count(*) FROM progress_values)) RETURNING id, seen, snapshot_count",
            &[],
        )
        .unwrap();
    assert_eq!(
        values
            .rows
            .iter()
            .map(|row| row["seen"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );
    assert!(values
        .rows
        .iter()
        .all(|row| row["snapshot_count"] == Value::Int(0)));

    root.sql(
        "INSERT INTO progress_defaults(id) VALUES (1), (2), (3)",
        &[],
    )
    .unwrap();
    let defaults = root
        .sql("SELECT seen FROM progress_defaults ORDER BY id", &[])
        .unwrap();
    assert_eq!(
        defaults
            .rows
            .iter()
            .map(|row| row["seen"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );

    let conflicts = root
        .sql(
            "INSERT INTO progress_conflicts VALUES (1, 0), (2, 0), (3, 0) ON CONFLICT (id) DO UPDATE SET seen = visible_command_sum('progress_conflicts') RETURNING id, seen",
            &[],
        )
        .unwrap();
    assert_eq!(
        conflicts
            .rows
            .iter()
            .map(|row| row["seen"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(60), Value::Int(110), Value::Int(200)]
    );

    root.sql(
        "UPDATE progress_updates SET seen = visible_command_sum('progress_updates')",
        &[],
    )
    .unwrap();
    let updates = root
        .sql("SELECT seen FROM progress_updates ORDER BY id", &[])
        .unwrap();
    assert_eq!(
        updates
            .rows
            .iter()
            .map(|row| row["seen"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(60), Value::Int(110), Value::Int(200)]
    );
}

#[test]
fn delete_returning_preserves_snapshot_subqueries_and_command_progress_for_volatile_calls() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        Arc::new(Engine::open(&directory.path().join("returning-volatile-timing.db")).unwrap());
    root.sql(
        "CREATE TABLE timing_target (id INTEGER PRIMARY KEY); CREATE TABLE timing_locks (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO timing_target VALUES (1), (2), (3); INSERT INTO timing_locks VALUES (1, 9)",
        &[],
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let callback_engine = Arc::downgrade(&root);
    root.register_scalar_function_with_options(
        "remaining_timing_rows",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            let engine = callback_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("RETURNING timing engine was dropped".into())
            })?;
            let mut remaining = 0_i64;
            for doc_id in 1..=3 {
                if engine.get_document("timing_target", doc_id)?.is_some() {
                    remaining += 1;
                }
            }
            Ok(Value::Int(remaining))
        },
    )
    .unwrap();
    let sql_calls = Arc::new(AtomicUsize::new(0));
    let observed_sql_calls = Arc::clone(&sql_calls);
    let sql_callback_engine = Arc::downgrade(&root);
    root.register_scalar_function_with_options(
        "remaining_timing_rows_sql",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            observed_sql_calls.fetch_add(1, Ordering::SeqCst);
            let engine = sql_callback_engine.upgrade().ok_or_else(|| {
                uqa_sql::SQLError::Internal("RETURNING timing engine was dropped".into())
            })?;
            Ok(engine
                .sql("SELECT count(*) AS remaining FROM timing_target", &[])?
                .rows[0]["remaining"]
                .clone())
        },
    )
    .unwrap();
    root.register_scalar_function_with_options(
        "unreachable_timing_boom",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        |_args: &[Value]| {
            Err(uqa_sql::SQLError::Internal(
                "unreachable RETURNING branch was evaluated".into(),
            ))
        },
    )
    .unwrap();

    let result = root
        .sql(
            "DELETE FROM timing_target RETURNING id, remaining_timing_rows() AS remaining, (SELECT count(*) FROM timing_target) AS snapshot_count, (SELECT remaining_timing_rows() WHERE timing_target.id > 0) AS nested_remaining, (SELECT remaining_timing_rows_sql() WHERE timing_target.id > 0) AS nested_sql_remaining, CASE WHEN timing_target.id = 999 THEN (SELECT unreachable_timing_boom() FROM timing_locks WHERE id = 1 FOR UPDATE) ELSE 0 END AS unreachable, (SELECT value FROM timing_locks WHERE id = 1 FOR SHARE) AS locked_value",
            &[],
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(sql_calls.load(Ordering::SeqCst), 3);
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["remaining"], Value::Int(2));
    assert_eq!(result.rows[1]["remaining"], Value::Int(1));
    assert_eq!(result.rows[2]["remaining"], Value::Int(0));
    assert_eq!(result.rows[0]["nested_remaining"], Value::Int(2));
    assert_eq!(result.rows[1]["nested_remaining"], Value::Int(1));
    assert_eq!(result.rows[2]["nested_remaining"], Value::Int(0));
    assert_eq!(result.rows[0]["nested_sql_remaining"], Value::Int(2));
    assert_eq!(result.rows[1]["nested_sql_remaining"], Value::Int(1));
    assert_eq!(result.rows[2]["nested_sql_remaining"], Value::Int(0));
    assert!(result
        .rows
        .iter()
        .all(|row| row["snapshot_count"] == Value::Int(3)));
    assert!(result
        .rows
        .iter()
        .all(|row| row["locked_value"] == Value::Int(9)));
    assert!(result
        .rows
        .iter()
        .all(|row| row["unreachable"] == Value::Int(0)));
}

#[test]
fn delete_returning_locking_subquery_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("delete-returning-lock-order.db")).unwrap();
    root.sql(
        "CREATE TABLE delete_returning_source (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE delete_returning_target (id INTEGER PRIMARY KEY); INSERT INTO delete_returning_source VALUES (1, 40); INSERT INTO delete_returning_target VALUES (1)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let deleter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM delete_returning_source WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let delete_thread = std::thread::spawn(move || {
        done_tx
            .send(deleter.sql(
                "DELETE FROM delete_returning_target WHERE id = 1 RETURNING id, (SELECT value FROM delete_returning_source AS source WHERE source.id = delete_returning_target.id FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE delete_returning_source SET value = value + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    delete_thread.join().unwrap();
    assert_eq!(result.rows[0]["locked_value"], Value::Int(41));
}

#[test]
fn merge_returning_locking_subquery_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merge-returning-lock-order.db")).unwrap();
    root.sql(
        "CREATE TABLE merge_returning_locks (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE merge_returning_target (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE merge_returning_input (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO merge_returning_locks VALUES (1, 50); INSERT INTO merge_returning_target VALUES (1, 0); INSERT INTO merge_returning_input VALUES (1, 9)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let merger = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM merge_returning_locks WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let merge_thread = std::thread::spawn(move || {
        done_tx
            .send(merger.sql(
                "MERGE INTO merge_returning_target AS target USING merge_returning_input AS input ON target.id = input.id WHEN MATCHED THEN UPDATE SET value = input.value RETURNING target.id AS id, (SELECT value FROM merge_returning_locks AS locks WHERE locks.id = target.id FOR UPDATE) AS locked_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE merge_returning_locks SET value = value + 1 WHERE id = 1",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    merge_thread.join().unwrap();
    assert_eq!(result.rows[0]["locked_value"], Value::Int(51));
}

#[test]
fn insert_conflict_wait_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-conflict-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE conflict_writer_order (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO conflict_writer_order VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conflict_writer_order WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO conflict_writer_order VALUES (1, 9) ON CONFLICT DO NOTHING",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("DELETE FROM conflict_writer_order WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let insert_result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(insert_result.affected_rows, 1);
}

#[test]
fn foreign_key_wait_precedes_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("foreign-key-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE parent_writer_order (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE child_writer_order (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent_writer_order(id)); INSERT INTO parent_writer_order VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM parent_writer_order WHERE id = 1 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql("INSERT INTO child_writer_order VALUES (10, 1)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE parent_writer_order SET value = 1 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
}

#[test]
fn multirow_insert_foreign_keys_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("multirow-insert-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE multirow_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE multirow_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES multirow_fk_parent(id)); INSERT INTO multirow_fk_parent VALUES (1, 0), (2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM multirow_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql("INSERT INTO multirow_fk_child VALUES (10, 1), (20, 2)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE multirow_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM multirow_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn multirow_insert_conflicts_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("multirow-insert-conflict-writer-order.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE multirow_conflict_target (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO multirow_conflict_target VALUES (2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM multirow_conflict_target WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO multirow_conflict_target VALUES (1, 1), (2, 2) ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql(
            "UPDATE multirow_conflict_target SET value = 9 WHERE id = 2",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    let rows = root
        .sql(
            "SELECT id, value FROM multirow_conflict_target ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows[0]["value"], Value::Int(1));
    assert_eq!(rows.rows[1]["value"], Value::Int(2));
}

#[test]
fn multirow_update_foreign_keys_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("multirow-update-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE update_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE update_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES update_fk_parent(id)); INSERT INTO update_fk_parent VALUES (1, 0), (2, 0), (3, 0); INSERT INTO update_fk_child VALUES (1, 3), (2, 3)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM update_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql(
                "UPDATE update_fk_child SET parent_id = id WHERE id IN (1, 2)",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE update_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    let rows = root
        .sql("SELECT parent_id FROM update_fk_child ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows[0]["parent_id"], Value::Int(1));
    assert_eq!(rows.rows[1]["parent_id"], Value::Int(2));
}

#[test]
fn delete_cascade_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("delete-cascade-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE delete_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE delete_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES delete_parent(id) ON DELETE CASCADE, value INTEGER); INSERT INTO delete_parent VALUES (1, 0), (2, 0); INSERT INTO delete_child VALUES (10, 1, 0), (20, 2, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let deleter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM delete_child WHERE id = 20 FOR UPDATE", &[])
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let delete_thread = std::thread::spawn(move || {
        done_tx
            .send(deleter.sql("DELETE FROM delete_parent WHERE id IN (1, 2)", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE delete_child SET value = 1 WHERE id = 20", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    delete_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM delete_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(0)
    );
}

#[test]
fn insert_select_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("insert-select-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE select_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE select_fk_source (id INTEGER PRIMARY KEY, parent_id INTEGER); CREATE TABLE select_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES select_fk_parent(id)); INSERT INTO select_fk_parent VALUES (1, 0), (2, 0); INSERT INTO select_fk_source VALUES (10, 1), (20, 2)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    inserter.sql("SET work_mem TO '1B'", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM select_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO select_fk_child SELECT id, parent_id FROM select_fk_source ORDER BY id",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE select_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM select_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn conflict_update_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("conflict-update-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE conflict_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE conflict_fk_target (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES conflict_fk_parent(id)); INSERT INTO conflict_fk_parent VALUES (1, 0), (2, 0); INSERT INTO conflict_fk_target VALUES (1, 1)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conflict_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO conflict_fk_target VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET parent_id = 2",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE conflict_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT parent_id FROM conflict_fk_target WHERE id = 1", &[],)
            .unwrap()
            .rows[0]["parent_id"],
        Value::Int(2)
    );
}

#[test]
fn merge_insert_dependencies_precede_the_data_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merge-insert-fk-writer-order.db")).unwrap();
    root.sql(
        "CREATE TABLE merge_fk_parent (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE merge_fk_source (id INTEGER PRIMARY KEY, parent_id INTEGER); CREATE TABLE merge_fk_child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES merge_fk_parent(id)); INSERT INTO merge_fk_parent VALUES (1, 0), (2, 0); INSERT INTO merge_fk_source VALUES (10, 1), (20, 2)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let merger = root.new_session().unwrap();
    merger.sql("SET work_mem TO '1B'", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM merge_fk_parent WHERE id = 2 FOR UPDATE",
            &[],
        )
        .unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let merge_thread = std::thread::spawn(move || {
        done_tx
            .send(merger.sql(
                "MERGE INTO merge_fk_child AS c USING merge_fk_source AS s ON c.id = s.id WHEN NOT MATCHED THEN INSERT (id, parent_id) VALUES (s.id, s.parent_id)",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE merge_fk_parent SET value = 1 WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    merge_thread.join().unwrap();
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM merge_fk_child", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
}

#[test]
fn create_table_as_locking_select_rechecks_concurrent_updates() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("ctas-lock-recheck.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let creator = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let create_thread = std::thread::spawn(move || {
        done_tx
            .send(creator.sql(
                "CREATE TABLE snap AS SELECT id, balance FROM accounts WHERE id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 555 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    create_thread.join().unwrap();
    let snapped = root
        .sql("SELECT balance FROM snap WHERE id = 1", &[])
        .unwrap();
    assert_eq!(snapped.rows[0].get("balance"), Some(&Value::Int(555)));
}
