//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn blocking_wait_rechecks_predicate_against_a_fresh_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("wait-recheck-filter.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, balance FROM accounts WHERE balance = 100 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 99 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert!(result.rows.is_empty(), "stale rows: {:?}", result.rows);
}

#[test]
fn blocking_wait_returns_the_current_row_version_when_it_still_qualifies() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("wait-recheck-value.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, balance FROM accounts WHERE id = 1 AND balance >= 100 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 101 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(101)));
}

#[test]
fn row_changed_after_scan_is_rechecked_without_a_lock_wait() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("scan-before-lock.db")).unwrap();
    seed_accounts(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "row_lock_scan_gate",
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
                "SELECT id, balance, row_lock_scan_gate() AS gate FROM accounts WHERE id = 1 ORDER BY gate FOR UPDATE",
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
fn stronger_later_lock_scope_refetches_after_a_key_share_compatible_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("lock-strength-cache-scope.db")).unwrap();
    root.sql(
        "CREATE TABLE lock_strength_scope (id INTEGER PRIMARY KEY, value INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO lock_strength_scope VALUES (1, 10)", &[])
        .unwrap();
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "lock_strength_scope_gate",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            if callback_calls.fetch_add(1, Ordering::SeqCst) == 0 {
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
                "SELECT (SELECT value FROM lock_strength_scope WHERE id = 1 FOR KEY SHARE) AS key_share_value, lock_strength_scope_gate() AS gate, (SELECT value FROM lock_strength_scope WHERE id = 1 FOR UPDATE) AS update_value",
                &[],
            ))
            .unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    updater
        .sql(
            "UPDATE lock_strength_scope SET value = 99 WHERE id = 1",
            &[],
        )
        .unwrap();
    gate.wait();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    reader_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("key_share_value"), Some(&Value::Int(10)));
    assert_eq!(result.rows[0].get("update_value"), Some(&Value::Int(99)));
}

#[test]
fn row_lock_recheck_keeps_the_original_unmarked_join_partner() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("epq-join-partner.db")).unwrap();
    root.sql(
        "CREATE TABLE epq_target (id INTEGER PRIMARY KEY, match_key INTEGER); CREATE TABLE epq_source (match_key INTEGER, label TEXT); INSERT INTO epq_target VALUES (1, 1); INSERT INTO epq_source VALUES (1, 'one'), (2, 'two')",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("UPDATE epq_target SET match_key = 2 WHERE id = 1", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT target.match_key, source.label FROM epq_target AS target JOIN epq_source AS source ON target.match_key = source.match_key WHERE target.id = 1 FOR UPDATE OF target",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert!(result.rows.is_empty());
}

#[test]
fn row_lock_recheck_keeps_each_original_values_partner() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("epq-values-partner.db")).unwrap();
    root.sql(
        "CREATE TABLE epq_values_target (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO epq_values_target VALUES (1, 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("UPDATE epq_values_target SET v = 99 WHERE id = 1", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT target.id, target.v, q.x FROM epq_values_target AS target CROSS JOIN (VALUES (1), (2)) AS q(x) ORDER BY q.x FOR UPDATE OF target",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("v"), Some(&Value::Int(99)));
    assert_eq!(result.rows[1].get("v"), Some(&Value::Int(99)));
    assert_eq!(result.rows[0].get("x"), Some(&Value::Int(1)));
    assert_eq!(result.rows[1].get("x"), Some(&Value::Int(2)));
}

#[test]
fn update_from_recheck_keeps_the_original_source_tuple() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("epq-update-from-source.db")).unwrap();
    root.sql(
        "CREATE TABLE epq_update_target (id INTEGER PRIMARY KEY, match_key INTEGER, value TEXT); CREATE TABLE epq_update_source (match_key INTEGER, value TEXT); INSERT INTO epq_update_target VALUES (1, 1, 'old'); INSERT INTO epq_update_source VALUES (1, 'one'), (2, 'two')",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "UPDATE epq_update_target SET match_key = 2 WHERE id = 1",
            &[],
        )
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "UPDATE epq_update_target AS target SET value = source.value FROM epq_update_source AS source WHERE target.match_key = source.match_key RETURNING target.match_key, target.value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 0);
    assert!(result.rows.is_empty());
    let final_row = root
        .sql(
            "SELECT match_key, value FROM epq_update_target WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(final_row.rows[0].get("match_key"), Some(&Value::Int(2)));
    assert_eq!(
        final_row.rows[0].get("value"),
        Some(&Value::Str("old".into()))
    );
}

#[test]
fn delete_using_recheck_keeps_the_original_source_tuple() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("epq-delete-using-source.db")).unwrap();
    root.sql(
        "CREATE TABLE epq_delete_target (id INTEGER PRIMARY KEY, match_key INTEGER); CREATE TABLE epq_delete_source (match_key INTEGER); INSERT INTO epq_delete_target VALUES (1, 1); INSERT INTO epq_delete_source VALUES (1), (2)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "UPDATE epq_delete_target SET match_key = 2 WHERE id = 1",
            &[],
        )
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "DELETE FROM epq_delete_target AS target USING epq_delete_source AS source WHERE target.match_key = source.match_key RETURNING target.id, target.match_key",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 0);
    assert!(result.rows.is_empty());
    let final_row = root
        .sql("SELECT match_key FROM epq_delete_target WHERE id = 1", &[])
        .unwrap();
    assert_eq!(final_row.rows[0].get("match_key"), Some(&Value::Int(2)));
}

#[test]
fn pure_lock_wait_keeps_concurrent_inserts_out_of_the_command_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("wait-command-snapshot.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql("SELECT id FROM accounts ORDER BY id FOR UPDATE", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    inserter
        .sql(
            "INSERT INTO accounts (id, owner, balance) VALUES (4, 'dana', 400)",
            &[],
        )
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn explicit_transaction_wait_rechecks_with_a_fresh_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("explicit-wait-recheck.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    waiter.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, balance FROM accounts WHERE balance = 100 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 99 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert!(result.rows.is_empty(), "stale rows: {:?}", result.rows);
}

fn assert_builtin_null_rejection(root: &Engine) {
    let result = root
        .sql(
            "SELECT base.id, detail.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE detail.id IS NOT NULL FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);

    let unqualified = root
        .sql(
            "SELECT base.id, detail.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE base_id IS NOT NULL FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(unqualified.rows.len(), 1);

    let direct_boolean = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE detail.permitted FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(direct_boolean.rows.len(), 1);

    let strict_builtin = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE abs(detail.id) > 0 AND base.id > 0 FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(strict_builtin.rows.len(), 1);

    let not_between_rows = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE base.id NOT BETWEEN detail.id AND 1 ORDER BY base.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        not_between_rows
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
    let not_between_lock = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE base.id NOT BETWEEN detail.id AND 1 FOR UPDATE OF detail",
            &[],
        )
        .unwrap_err();
    assert!(
        not_between_lock
            .to_string()
            .contains("nullable side of an outer join"),
        "{not_between_lock}"
    );

    let between = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE base.id BETWEEN 0 AND detail.id FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(between.rows.len(), 1);
    let between_symmetric = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE base.id BETWEEN SYMMETRIC 0 AND detail.id FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(between_symmetric.rows.len(), 1);
}

fn assert_function_null_rejection(root: &Engine) {
    root.sql(
        "CREATE FUNCTION row_lock_strict_identity(input_value integer) RETURNS integer AS $$ BEGIN RETURN input_value; END; $$ LANGUAGE plpgsql STRICT",
        &[],
    )
    .unwrap();
    let strict_routine = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE row_lock_strict_identity(detail.id) > 0 FOR UPDATE OF detail",
            &[],
        )
        .unwrap();
    assert_eq!(strict_routine.rows.len(), 1);

    root.sql(
        "CREATE FUNCTION row_lock_null_to_one(input_value integer) RETURNS integer AS $$ BEGIN RETURN coalesce(input_value, 1); END; $$ LANGUAGE plpgsql",
        &[],
    )
    .unwrap();
    let non_strict_routine = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE row_lock_null_to_one(detail.id) > 0 FOR UPDATE OF detail",
            &[],
        )
        .unwrap_err();
    assert!(
        non_strict_routine
            .to_string()
            .contains("nullable side of an outer join"),
        "{non_strict_routine}"
    );

    let non_strict_builtin = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON detail.base_id = base.id WHERE coalesce(detail.id, 1) > 0 FOR UPDATE OF detail",
            &[],
        )
        .unwrap_err();
    assert!(
        non_strict_builtin
            .to_string()
            .contains("nullable side of an outer join"),
        "{non_strict_builtin}"
    );

    let error = root
        .sql(
            "SELECT base.id, detail.id FROM base LEFT JOIN detail ON detail.base_id = base.id FOR UPDATE OF detail",
            &[],
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("nullable side of an outer join"),
        "{error}"
    );

    let in_list_error = root
        .sql(
            "SELECT base.id FROM base LEFT JOIN detail ON FALSE WHERE 1 IN (detail.id, 1) AND base.v > 0 FOR UPDATE OF detail",
            &[],
        )
        .unwrap_err();
    assert!(
        in_list_error
            .to_string()
            .contains("nullable side of an outer join"),
        "{in_list_error}"
    );
}

#[test]
fn null_rejecting_where_permits_locking_the_nullable_side() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("null-rejecting-lock.db")).unwrap();
    root.sql("CREATE TABLE base (id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .unwrap();
    root.sql(
        "CREATE TABLE detail (id INTEGER PRIMARY KEY, base_id INTEGER, permitted BOOLEAN)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO base VALUES (1, 10), (2, 20)", &[])
        .unwrap();
    root.sql("INSERT INTO detail VALUES (7, 1, TRUE)", &[])
        .unwrap();
    assert_builtin_null_rejection(&root);
    assert_function_null_rejection(&root);
}

#[test]
fn order_by_limit_retains_the_original_candidate_after_a_concurrent_reorder() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("order-candidate-retained.db")).unwrap();
    root.sql(
        "CREATE TABLE ranked (id INTEGER PRIMARY KEY, rank INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO ranked VALUES (1, 1), (2, 2)", &[])
        .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM ranked WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, rank FROM ranked ORDER BY rank LIMIT 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE ranked SET rank = 3 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[0].get("rank"), Some(&Value::Int(3)));
}

#[test]
fn order_by_limit_surfaces_the_next_candidate_when_the_recheck_drops_the_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("order-candidate-dropped.db")).unwrap();
    root.sql(
        "CREATE TABLE ranked (id INTEGER PRIMARY KEY, rank INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO ranked VALUES (1, 1), (2, 2)", &[])
        .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM ranked WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, rank FROM ranked WHERE rank <= 2 ORDER BY rank LIMIT 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE ranked SET rank = 99 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[0].get("rank"), Some(&Value::Int(2)));
}

#[test]
fn blocking_wait_follows_a_primary_key_rewrite_to_the_successor_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("pk-rewrite-successor.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, balance FROM accounts WHERE balance = 100 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET id = 5 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(5)));
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(100)));
}

#[test]
fn key_share_lock_returns_snapshot_values_after_a_compatible_non_key_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("key-share-snapshot.db")).unwrap();
    seed_accounts(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let gate = Arc::new(Barrier::new(2));
    let callback_gate = Arc::clone(&gate);
    let (entered_tx, entered_rx) = mpsc::channel();
    root.register_scalar_function_with_options(
        "key_share_scan_gate",
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
                "SELECT id, balance, key_share_scan_gate() AS gate FROM accounts WHERE id = 1 ORDER BY gate FOR KEY SHARE",
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
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(100)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
