//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn offset_rows_are_locked_before_the_slice_is_applied() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("offset-locks.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    assert_eq!(
        ids(
            &holder,
            "SELECT id FROM accounts ORDER BY id OFFSET 1 LIMIT 1 FOR UPDATE"
        ),
        [2]
    );

    for id in [1, 2] {
        let error = waiter
            .sql(
                &format!("SELECT id FROM accounts WHERE id = {id} FOR UPDATE NOWAIT"),
                &[],
            )
            .unwrap_err();
        assert_eq!(sqlstate(&error), "55P03");
    }
    waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn duplicate_and_overlapping_clauses_merge_to_nowait_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("overlap.db")).unwrap();
    seed_accounts(&root);
    let blocker = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    blocker.sql("BEGIN", &[]).unwrap();
    blocker
        .sql("SELECT id FROM accounts WHERE id = 1 FOR KEY SHARE", &[])
        .unwrap();

    let error = contender
        .sql(
            "SELECT id FROM accounts WHERE id = 1
             FOR KEY SHARE OF accounts, accounts SKIP LOCKED
             FOR UPDATE OF accounts NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    blocker.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn locking_inside_a_streamed_derived_projection_is_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("streamed-derived-lock.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT * FROM (SELECT id FROM accounts FOR UPDATE) AS s WHERE id = 1",
            &[],
        )
        .unwrap();
    let error = contender
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn outer_row_mark_merges_into_the_streamed_derived_query() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merged-derived-lock.db")).unwrap();
    seed_accounts(&root);
    let blocker = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    blocker.sql("BEGIN", &[]).unwrap();
    blocker
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = contender
        .sql(
            "SELECT * FROM (SELECT id FROM accounts WHERE id = 1 FOR SHARE) AS s FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    blocker.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn direct_row_identity_barriers_match_postgresql_errors() {
    let engine = Engine::new();
    seed_accounts(&engine);
    engine
        .sql(
            "CREATE VIEW distinct_balances AS SELECT DISTINCT balance FROM accounts",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE VIEW aggregate_balances AS SELECT count(*) AS n FROM accounts",
            &[],
        )
        .unwrap();

    for sql in [
        "SELECT count(*) FROM accounts FOR UPDATE",
        "SELECT * FROM distinct_balances FOR SHARE",
        "SELECT * FROM aggregate_balances FOR UPDATE",
        "SELECT * FROM (SELECT id FROM accounts UNION ALL SELECT id FROM accounts) AS s FOR UPDATE",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
}

#[test]
fn set_operation_view_is_a_lock_barrier_and_nested_distinct_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("view-barriers.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE VIEW account_union AS
         SELECT id FROM accounts UNION ALL SELECT id FROM accounts",
        &[],
    )
    .unwrap();
    root.sql(
        "CREATE VIEW distinct_accounts AS SELECT DISTINCT id FROM accounts",
        &[],
    )
    .unwrap();
    root.sql(
        "CREATE VIEW wrapped_distinct_accounts AS SELECT id FROM distinct_accounts",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM account_union FOR UPDATE", &[])
        .unwrap();
    let distinct_error = holder
        .sql("SELECT id FROM wrapped_distinct_accounts FOR UPDATE", &[])
        .unwrap_err();
    assert_eq!(distinct_error.sqlstate(), Some("0A000"));

    waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn implicit_cte_locking_is_a_noop_but_explicit_of_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cte-barrier.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "WITH selected AS (SELECT id FROM accounts WHERE id = 1)
             SELECT id FROM selected FOR UPDATE",
            &[],
        )
        .unwrap();
    waiter
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    let error = holder
        .sql(
            "WITH selected AS (SELECT id FROM accounts WHERE id = 1)
             SELECT id FROM selected FOR UPDATE OF selected",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(error.to_string().contains("WITH query"));
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn alias_hides_the_base_relation_in_of_clause() {
    let engine = Engine::new();
    seed_accounts(&engine);
    let error = engine
        .sql("SELECT a.id FROM accounts AS a FOR UPDATE OF accounts", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P01"));
}

#[test]
fn row_lock_syntax_and_target_shape_errors_match_postgresql_18() {
    let engine = Engine::new();
    seed_accounts(&engine);
    engine
        .register_aggregate_function("row_lock_sum", RowLockSum::default)
        .unwrap();

    let qualified = engine
        .sql(
            "SELECT id FROM public.accounts FOR UPDATE OF public.accounts",
            &[],
        )
        .unwrap_err();
    assert_eq!(qualified.sqlstate(), Some("42601"));

    for sql in [
        "SELECT generate_series(1, 3) FOR UPDATE",
        "SELECT id FROM accounts ORDER BY generate_series(1, 2) FOR UPDATE",
        "SELECT row_lock_sum(balance) FROM accounts FOR UPDATE",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
}

#[test]
fn locking_ctes_execute_only_when_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cte-reachability.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "WITH unused AS (SELECT id FROM accounts FOR UPDATE) SELECT 1",
            &[],
        )
        .unwrap();
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .expect("an unreferenced SELECT CTE must not execute");

    holder
        .sql(
            "WITH locked AS (SELECT id FROM accounts WHERE id = 1 FOR UPDATE),
                  forwarded AS (SELECT id FROM locked)
             SELECT id FROM forwarded",
            &[],
        )
        .unwrap();
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn consumer_limits_lock_only_rows_they_pull() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("row-lock-demand.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE VIEW locked_account_ids AS SELECT id FROM accounts ORDER BY id FOR UPDATE",
        &[],
    )
    .unwrap();

    for sql in [
        "SELECT id FROM accounts ORDER BY id LIMIT 1 FOR UPDATE",
        "SELECT * FROM (SELECT id FROM accounts ORDER BY id FOR UPDATE) AS s LIMIT 1",
        "SELECT * FROM (SELECT id FROM accounts WHERE id >= 1 FOR UPDATE) AS s LIMIT 1",
        "WITH c AS (SELECT id FROM accounts ORDER BY id FOR UPDATE) SELECT * FROM c LIMIT 1",
        "SELECT * FROM locked_account_ids LIMIT 1",
    ] {
        assert_query_locks_only_one_row(&root, sql);
    }
}

#[test]
fn skip_locked_limit_does_not_lock_an_unconsumed_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("skip-locked-demand.db")).unwrap();
    seed_accounts(&root);
    let blocker = root.new_session().unwrap();
    let consumer = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    blocker.sql("BEGIN", &[]).unwrap();
    blocker
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    consumer.sql("BEGIN", &[]).unwrap();
    assert_eq!(
        ids(
            &consumer,
            "SELECT id FROM accounts ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED"
        ),
        [2]
    );
    let locked = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 2 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(locked.sqlstate(), Some("55P03"));
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .expect("LIMIT must not pull and lock row 3");
    consumer.sql("ROLLBACK", &[]).unwrap();
    blocker.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn outer_nowait_is_merged_into_a_view_row_mark() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("view-nowait-merge.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE VIEW shared_account AS SELECT id FROM accounts WHERE id = 1 FOR SHARE",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    let cancel = contender.cancellation_token();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(contender.sql("SELECT * FROM shared_account FOR UPDATE NOWAIT", &[]))
            .unwrap();
    });
    let result = if let Ok(result) = done_rx.recv_timeout(Duration::from_millis(250)) {
        result
    } else {
        cancel.cancel();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap()
    };
    let error = result.expect_err("the outer NOWAIT must reach the view's base-table lock");
    assert_eq!(error.sqlstate(), Some("55P03"));
    waiting_thread.join().unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn explicit_transaction_uses_a_new_read_committed_snapshot_per_statement() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("read-committed-statements.db")).unwrap();
    seed_accounts(&root);
    let reader = root.new_session().unwrap();
    let writer = root.new_session().unwrap();
    reader.sql("BEGIN", &[]).unwrap();
    let initial = reader
        .sql("SELECT balance FROM accounts WHERE id = 1", &[])
        .unwrap();
    assert_eq!(initial.rows[0].get("balance"), Some(&Value::Int(100)));
    writer
        .sql("UPDATE accounts SET balance = 444 WHERE id = 1", &[])
        .unwrap();
    let current = reader
        .sql("SELECT balance FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    assert_eq!(current.rows[0].get("balance"), Some(&Value::Int(444)));
    reader.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn volatile_projection_retry_matches_postgresql_tuple_rechecks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("volatile-row-lock-retry.db")).unwrap();
    seed_accounts(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    root.register_scalar_function_with_options(
        "row_lock_retry_tick",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            Ok(Value::Int(
                observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
            ))
        },
    )
    .unwrap();

    run_volatile_lock_wait(
        &root,
        &calls,
        "SELECT id FROM accounts WHERE id = 3 FOR UPDATE",
        false,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    run_volatile_lock_wait(
        &root,
        &calls,
        "UPDATE accounts SET balance = balance WHERE id = 3",
        false,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    let changed = run_volatile_lock_wait(
        &root,
        &calls,
        "UPDATE accounts SET balance = balance + 1 WHERE id = 3",
        false,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let changed_row = changed
        .rows
        .iter()
        .find(|row| row.get("id") == Some(&Value::Int(3)))
        .unwrap();
    assert_eq!(changed_row.get("balance"), Some(&Value::Int(301)));
    assert_eq!(changed_row.get("tick"), Some(&Value::Int(4)));

    run_volatile_lock_wait(
        &root,
        &calls,
        "SELECT id FROM accounts WHERE id = 3 FOR UPDATE",
        true,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn volatile_projection_is_not_deduplicated_for_duplicate_input_rows() {
    let engine = Engine::new();
    seed_accounts(&engine);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function_with_options(
            "row_lock_duplicate_tick",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_args: &[Value]| {
                Ok(Value::Int(
                    observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
                ))
            },
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT a.id, row_lock_duplicate_tick() AS tick
             FROM accounts AS a
             CROSS JOIN (VALUES (1), (1)) AS duplicate(n)
             WHERE a.id = 1
             ORDER BY duplicate.n
             FOR UPDATE OF a",
            &[],
        )
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["tick"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn nowait_evaluates_scalar_targets_at_the_postgresql_lockrows_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("nowait-projection.db")).unwrap();
    seed_accounts(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    root.register_scalar_function_with_options(
        "nowait_projection_tick",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Int(1))
        },
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = contender
        .sql(
            "SELECT nowait_projection_tick() FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    calls.store(0, Ordering::SeqCst);
    let error = contender
        .sql(
            "SELECT nowait_projection_tick() FROM accounts ORDER BY id FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    calls.store(0, Ordering::SeqCst);
    let error = contender
        .sql(
            "SELECT nowait_projection_tick() AS tick FROM accounts ORDER BY tick FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn on_conflict_do_nothing_releases_only_its_transient_key_share_lock() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("conflict-transient-key-share.db")).unwrap();
    root.sql(
        "CREATE TABLE conflict_target (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO conflict_target VALUES (1, 'old')",
        &[],
    )
    .unwrap();
    let inserter = root.new_session().unwrap();
    let contender = root.new_session().unwrap();
    inserter.sql("BEGIN", &[]).unwrap();
    let ignored = inserter
        .sql(
            "INSERT INTO conflict_target VALUES (1, 'ignored') ON CONFLICT DO NOTHING",
            &[],
        )
        .unwrap();
    assert_eq!(ignored.affected_rows, 0);
    contender
        .sql(
            "SELECT id FROM conflict_target WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    inserter.sql("ROLLBACK", &[]).unwrap();

    let holder = root.new_session().unwrap();
    let blocked = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM conflict_target WHERE id = 1 FOR KEY SHARE",
            &[],
        )
        .unwrap();
    holder
        .sql(
            "INSERT INTO conflict_target VALUES (1, 'ignored') ON CONFLICT DO NOTHING",
            &[],
        )
        .unwrap();
    let error = blocked
        .sql(
            "SELECT id FROM conflict_target WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn mutation_predicates_are_not_repeated_without_a_lock_wait() {
    let engine = Engine::new();
    seed_accounts(&engine);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function_with_options(
            "row_lock_mutation_predicate",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_args: &[Value]| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Bool(true))
            },
        )
        .unwrap();
    let updated = engine
        .sql(
            "UPDATE accounts SET balance = balance + 1 WHERE row_lock_mutation_predicate()",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    engine
        .sql(
            "CREATE TABLE row_lock_mutation_target (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE row_lock_mutation_source (id INTEGER PRIMARY KEY); INSERT INTO row_lock_mutation_target VALUES (1, 0); INSERT INTO row_lock_mutation_source VALUES (1)",
            &[],
        )
        .unwrap();
    calls.store(0, Ordering::SeqCst);
    let updated = engine
        .sql(
            "UPDATE row_lock_mutation_target AS target SET value = value + 1 FROM row_lock_mutation_source AS source WHERE target.id = source.id AND row_lock_mutation_predicate()",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    calls.store(0, Ordering::SeqCst);
    let deleted = engine
        .sql(
            "DELETE FROM row_lock_mutation_target AS target USING row_lock_mutation_source AS source WHERE target.id = source.id AND row_lock_mutation_predicate()",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    calls.store(0, Ordering::SeqCst);
    let deleted = engine
        .sql(
            "DELETE FROM accounts WHERE row_lock_mutation_predicate()",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn generated_key_dependency_update_conflicts_with_key_share() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("generated-key-lock.db")).unwrap();
    root.sql(
        "CREATE TABLE generated_lock (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (source + 10) STORED UNIQUE)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO generated_lock (id, source) VALUES (1, 1)", &[])
        .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql(
            "SELECT id FROM generated_lock WHERE id = 1 FOR KEY SHARE",
            &[],
        )
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE generated_lock SET source = 2 WHERE id = 1", &[]))
            .unwrap();
    });
    let early = done_rx.recv_timeout(Duration::from_millis(250));
    holder.sql("ROLLBACK", &[]).unwrap();
    let (was_blocked, outcome) = receive_after_unblock(early, &done_rx, Duration::from_secs(2));
    update_thread.join().unwrap();
    assert!(was_blocked);
    outcome.unwrap();
}

#[test]
fn delete_cascade_locks_the_child_row_before_removing_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("cascade-lock.db")).unwrap();
    root.sql("CREATE TABLE parents (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    root.sql(
        "CREATE TABLE children (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE
        )",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO parents VALUES (1)", &[]).unwrap();
    root.sql("INSERT INTO children VALUES (10, 1)", &[])
        .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("DELETE FROM parents WHERE id = 1", &[]).unwrap();

    let error = waiter
        .sql(
            "SELECT id FROM children WHERE id = 10 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn memory_target_engine_open_skips_filesystem_identity() {
    let engine = Engine::open(std::path::Path::new(":memory:")).unwrap();
    seed_accounts(&engine);
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert!(!std::path::Path::new(":memory:.uqa-locks").exists());
}
