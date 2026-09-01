//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn ranked_text_match_lock_rechecks_the_changed_document() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("fts-lock-recheck.db")).unwrap();
    root.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, hits INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    root.sql(
        "INSERT INTO docs VALUES (1, 'alpha ranking test', 10), (2, 'alpha other', 20)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM docs WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, hits, _score FROM docs WHERE text_match(body, 'alpha') ORDER BY _score DESC, id FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE docs SET hits = 99 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 2);
    let hits_of = |id: i64| {
        result
            .rows
            .iter()
            .find(|row| row.get("id") == Some(&Value::Int(id)))
            .and_then(|row| row.get("hits").cloned())
    };
    assert_eq!(hits_of(1), Some(Value::Int(99)));
    assert_eq!(hits_of(2), Some(Value::Int(20)));
}

#[test]
fn blocking_wait_drops_a_candidate_the_holder_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("wait-deleted-candidate.db")).unwrap();
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
            .send(waiter.sql("SELECT id FROM accounts ORDER BY id FOR UPDATE", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("DELETE FROM accounts WHERE id = 1", &[])
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
        vec![Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn self_join_recheck_substitutes_the_committed_image_for_every_alias() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("self-join-recheck.db")).unwrap();
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
                "SELECT a.balance AS a_balance, b.balance AS b_balance FROM accounts a JOIN accounts b ON a.id = b.id WHERE a.id = 1 FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE accounts SET balance = 999 WHERE id = 1", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("a_balance"), Some(&Value::Int(999)));
    assert_eq!(result.rows[0].get("b_balance"), Some(&Value::Int(999)));
}

#[test]
fn nested_transaction_error_rolls_back_only_the_nested_frame() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("nested-frame-abort.db")).unwrap();
    seed_accounts(&root);
    let session = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    session.sql("BEGIN", &[]).unwrap();
    session
        .sql(
            "INSERT INTO accounts (id, owner, balance) VALUES (4, 'dana', 400)",
            &[],
        )
        .unwrap();
    session
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    session.sql("BEGIN", &[]).unwrap();
    session
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();
    session
        .sql("SELECT id FROM nonexistent_relation", &[])
        .unwrap_err();
    // The nested frame is aborted; the outer frame keeps its work and locks.
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    session.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(session.transaction_depth(), 1);
    let rows = session
        .sql("SELECT id FROM accounts WHERE id = 4", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1, "outer frame insert must survive");
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 2 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    session.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        root.sql("SELECT id FROM accounts WHERE id = 4", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn nested_frame_rollback_releases_locks_taken_before_an_inner_savepoint() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("nested-frame-savepoint.db")).unwrap();
    seed_accounts(&root);
    let session = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    session.sql("BEGIN", &[]).unwrap();
    session.sql("BEGIN", &[]).unwrap();
    session
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    session.sql("SAVEPOINT s", &[]).unwrap();
    session.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(session.transaction_depth(), 1);
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    session.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn failed_savepoint_command_aborts_the_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("failed-release.db")).unwrap();
    seed_accounts(&root);
    let session = root.new_session().unwrap();
    session.sql("BEGIN", &[]).unwrap();
    session.sql("RELEASE SAVEPOINT missing", &[]).unwrap_err();
    let error = session.sql("SELECT 1", &[]).unwrap_err();
    assert_eq!(sqlstate(&error), "25P02");
    session.sql("ROLLBACK", &[]).unwrap();
    session.sql("SELECT 1", &[]).unwrap();
}

#[test]
fn typed_writes_are_rejected_inside_an_aborted_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("typed-write-aborted.db")).unwrap();
    seed_accounts(&root);
    root.begin().unwrap();
    root.sql("UPDATE accounts SET balance = 1 WHERE id = 1", &[])
        .unwrap();
    root.sql("SELECT 1/0", &[]).unwrap_err();
    let mut document = std::collections::BTreeMap::new();
    document.insert("id".to_string(), Value::Int(42));
    document.insert("owner".to_string(), Value::Str("ghost".into()));
    document.insert("balance".to_string(), Value::Int(0));
    let error = root.add_document("accounts", 42, document).unwrap_err();
    assert_eq!(sqlstate(&error), "25P02");
    root.rollback().unwrap();
    assert!(root
        .sql("SELECT id FROM accounts WHERE id = 42", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        root.sql("SELECT balance FROM accounts WHERE id = 1", &[])
            .unwrap()
            .rows[0]
            .get("balance"),
        Some(&Value::Int(100))
    );
}

#[test]
fn typed_begin_defers_the_writer_until_row_dependencies_are_locked() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("typed-writer-deadlock.db")).unwrap();
    seed_accounts(&root);
    let sql_session = root.new_session().unwrap();
    let typed_session = root.new_session().unwrap();
    sql_session.sql("BEGIN", &[]).unwrap();
    sql_session
        .sql("SELECT id FROM accounts WHERE id = 2 FOR UPDATE", &[])
        .unwrap();
    typed_session.begin().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let typed_thread = std::thread::spawn(move || {
        let outcome = typed_session.sql(
            "UPDATE accounts SET balance = balance + 1 WHERE id IN (1, 2)",
            &[],
        );
        done_tx.send(outcome.map(|_| ())).unwrap();
        typed_session.rollback().ok();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(300)).is_err());
    let sql_outcome = sql_session.sql("UPDATE accounts SET balance = 5 WHERE id = 2", &[]);
    assert!(sql_outcome.is_ok(), "{sql_outcome:?}");
    sql_session.sql("ROLLBACK", &[]).unwrap();
    let typed_outcome = done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    typed_thread.join().unwrap();
    assert!(typed_outcome.is_ok(), "{typed_outcome:?}");
}

#[test]
fn merge_treats_a_target_deleted_during_the_wait_as_not_matched() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("merge-deleted-target.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let merger = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("DELETE FROM accounts WHERE id = 1", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let merge_thread = std::thread::spawn(move || {
        done_tx
            .send(merger.sql(
                "MERGE INTO accounts t USING (SELECT 1 AS id, 'zed' AS owner, 700 AS balance) s ON t.id = s.id WHEN MATCHED THEN UPDATE SET balance = s.balance WHEN NOT MATCHED THEN INSERT (id, owner, balance) VALUES (s.id, s.owner, s.balance)",
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
    merge_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    let rows = root
        .sql("SELECT owner, balance FROM accounts WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("owner"), Some(&Value::Str("zed".into())));
    assert_eq!(rows[0].get("balance"), Some(&Value::Int(700)));
}

#[test]
fn full_path_non_key_update_does_not_conflict_with_key_share() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("full-update-key-share.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 1 FOR KEY SHARE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql(
                "UPDATE accounts SET balance = balance + 1 WHERE id = 1 RETURNING balance",
                &[],
            ))
            .unwrap();
    });
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("a non-key UPDATE must not wait for FOR KEY SHARE")
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.rows[0].get("balance"), Some(&Value::Int(101)));
    holder.sql("COMMIT", &[]).unwrap();
}

#[test]
fn update_follows_a_primary_key_rewrite_committed_during_the_wait() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-follows-rewrite.db")).unwrap();
    root.sql(
        "CREATE TABLE t (id BIGINT PRIMARY KEY, u TEXT UNIQUE, v BIGINT)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO t VALUES (1, 'x', 10)", &[]).unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("UPDATE t SET id = 5 WHERE id = 1", &[]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE t SET v = 99 WHERE u = 'x'", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    let rows = root.sql("SELECT id, v FROM t", &[]).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::Int(5)));
    assert_eq!(rows[0].get("v"), Some(&Value::Int(99)));
}

#[test]
fn update_does_not_capture_a_row_reinserted_with_the_same_primary_key() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-reused-key.db")).unwrap();
    root.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO t VALUES (1, 'old')",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("DELETE FROM t WHERE id = 1", &[]).unwrap();
    holder
        .sql("INSERT INTO t VALUES (1, 'fresh')", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE t SET value = 'waiter' WHERE id = 1", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.affected_rows, 0);
    let rows = root
        .sql("SELECT value FROM t WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows[0].get("value"), Some(&Value::Str("fresh".into())));
}

#[test]
fn update_follows_an_ordered_chain_of_primary_key_rewrites() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-rewrite-chain.db")).unwrap();
    root.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, u TEXT UNIQUE, value TEXT); INSERT INTO t VALUES (3, 'x', 'old')",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("UPDATE t SET id = 2 WHERE id = 3", &[]).unwrap();
    holder.sql("UPDATE t SET id = 1 WHERE id = 2", &[]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE t SET value = 'waiter' WHERE u = 'x'", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    let rows = root.sql("SELECT id, value FROM t", &[]).unwrap().rows;
    assert_eq!(rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(rows[0].get("value"), Some(&Value::Str("waiter".into())));
}

#[test]
fn dml_statement_ctes_are_lockable_query_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("dml-cte-locking.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE audit (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    let inserted = root
        .sql(
            "WITH e AS (SELECT id, balance FROM accounts WHERE id <= 2) INSERT INTO audit SELECT id, balance FROM e FOR UPDATE",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.affected_rows, 2);
    let updated = root
        .sql(
            "WITH batch AS (SELECT id FROM accounts) UPDATE accounts SET balance = balance + 1 WHERE id IN (SELECT id FROM batch FOR UPDATE SKIP LOCKED)",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 3);
}

#[test]
fn derived_table_self_join_recheck_pins_each_inner_scan_separately() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("derived-self-join-recheck.db")).unwrap();
    root.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, parent INTEGER, v INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO t VALUES (1, 2, 10), (2, 1, 20)", &[])
        .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM t WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT a_id, b_id, a_v, b_v FROM (SELECT a.id AS a_id, b.id AS b_id, a.v AS a_v, b.v AS b_v FROM t a JOIN t b ON a.parent = b.id WHERE a.id = 1) s FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    match done_rx.recv_timeout(Duration::from_millis(150)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(outcome) => panic!(
            "derived self-join waiter finished before its tuple lock was released: {outcome:?}"
        ),
        Err(error) => panic!("derived self-join waiter disconnected: {error}"),
    }
    holder.sql("UPDATE t SET v = 99 WHERE id = 1", &[]).unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("a_id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[0].get("b_id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[0].get("a_v"), Some(&Value::Int(99)));
    assert_eq!(result.rows[0].get("b_v"), Some(&Value::Int(20)));
}

#[test]
fn spilled_derived_self_join_recheck_preserves_each_inner_scan_qualifier() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(
        &directory
            .path()
            .join("spilled-derived-self-join-recheck.db"),
    )
    .unwrap();
    root.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, parent INTEGER, v INTEGER)",
        &[],
    )
    .unwrap();
    root.sql(
        "INSERT INTO t VALUES (1, 0, 10), (2, 1, 20), (3, 1, 30)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    waiter.sql("SET work_mem TO '1B'", &[]).unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM t WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT a_id, b_id, a_v, b_v FROM (SELECT a.id AS a_id, b.id AS b_id, a.v AS a_v, b.v AS b_v FROM t a JOIN t b ON b.parent = a.id WHERE a.id = 1) s ORDER BY b_id FOR UPDATE OF s",
                &[],
            ))
            .unwrap();
    });
    match done_rx.recv_timeout(Duration::from_millis(150)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(outcome) => panic!("spilled derived self-join waiter finished before its tuple lock was released: {outcome:?}"),
        Err(error) => panic!("spilled derived self-join waiter disconnected: {error}"),
    }
    holder.sql("UPDATE t SET v = 99 WHERE id = 1", &[]).unwrap();
    holder.sql("COMMIT", &[]).unwrap();

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("a_id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[0].get("b_id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[0].get("a_v"), Some(&Value::Int(99)));
    assert_eq!(result.rows[0].get("b_v"), Some(&Value::Int(20)));
    assert_eq!(result.rows[1].get("a_id"), Some(&Value::Int(1)));
    assert_eq!(result.rows[1].get("b_id"), Some(&Value::Int(3)));
    assert_eq!(result.rows[1].get("a_v"), Some(&Value::Int(99)));
    assert_eq!(result.rows[1].get("b_v"), Some(&Value::Int(30)));
}

#[test]
fn chained_outer_joins_reduce_through_inner_join_conditions_before_locking() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("chained-outer-join-lock.db")).unwrap();
    for table in ["jt", "ju", "jv"] {
        root.sql(
            &format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY)"),
            &[],
        )
        .unwrap();
        root.sql(&format!("INSERT INTO {table} VALUES (1)"), &[])
            .unwrap();
    }
    let result = root
        .sql(
            "SELECT jt.id FROM jt LEFT JOIN ju ON jt.id = ju.id LEFT JOIN jv ON ju.id = jv.id WHERE jv.id IS NOT NULL FOR UPDATE",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let result = root
        .sql(
            "SELECT jt.id FROM jt LEFT JOIN ju ON jt.id = ju.id WHERE CAST(ju.id AS TEXT) = '1' FOR UPDATE OF ju",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let quoted = root
        .sql("SELECT id FROM jt AS \"A\" FOR UPDATE OF \"A\"", &[])
        .unwrap();
    assert_eq!(quoted.rows.len(), 1);
}

#[test]
fn ranked_text_match_recheck_drops_a_document_that_no_longer_matches() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("fts-lock-recheck-drop.db")).unwrap();
    root.sql(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, hits INTEGER)",
        &[],
    )
    .unwrap();
    root.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    root.sql(
        "INSERT INTO docs VALUES (1, 'alpha ranking test', 10), (2, 'alpha other', 20)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM docs WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "SELECT id, hits FROM docs WHERE text_match(body, 'alpha') ORDER BY _score DESC, id FOR UPDATE",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder
        .sql("UPDATE docs SET body = 'unrelated words' WHERE id = 1", &[])
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
        vec![Value::Int(2)]
    );
}

#[test]
fn locking_derived_table_with_a_scalar_subquery_stays_demand_driven() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("derived-subquery-demand.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    let result = holder
        .sql(
            "SELECT * FROM (SELECT id, (SELECT 1) AS one FROM accounts ORDER BY id FOR UPDATE) AS s LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(1)));
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn release_savepoint_keeps_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("release-keeps-locks.db")).unwrap();
    seed_accounts(&root);
    let holder = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM accounts WHERE id = 3 FOR UPDATE", &[])
        .unwrap();
    holder.sql("SAVEPOINT taken", &[]).unwrap();
    holder.sql("RELEASE SAVEPOINT taken", &[]).unwrap();
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    holder.sql("COMMIT", &[]).unwrap();
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
}

#[test]
fn locking_clause_inside_a_dml_cte_locks_its_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("dml-cte-locks-rows.db")).unwrap();
    seed_accounts(&root);
    root.sql(
        "CREATE TABLE audit (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let probe = root.new_session().unwrap();
    writer.sql("BEGIN", &[]).unwrap();
    let inserted = writer
        .sql(
            "WITH e AS (SELECT id, balance FROM accounts WHERE id <= 2 FOR UPDATE) INSERT INTO audit SELECT id, balance FROM e",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.affected_rows, 2);
    let error = probe
        .sql(
            "SELECT id FROM accounts WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(sqlstate(&error), "55P03");
    probe
        .sql(
            "SELECT id FROM accounts WHERE id = 3 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    writer.sql("COMMIT", &[]).unwrap();
}

#[test]
fn statements_after_a_savepoint_still_see_fresh_commits() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("savepoint-snapshot.db")).unwrap();
    seed_accounts(&root);
    let reader = root.new_session().unwrap();
    let writer = root.new_session().unwrap();
    reader.sql("BEGIN", &[]).unwrap();
    reader.sql("SAVEPOINT s", &[]).unwrap();
    assert_eq!(
        reader
            .sql("SELECT count(*) AS n FROM accounts", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(3)
    );
    writer
        .sql(
            "INSERT INTO accounts (id, owner, balance) VALUES (4, 'dana', 400)",
            &[],
        )
        .unwrap();
    // READ COMMITTED: a later statement of the same transaction sees the concurrent commit, even after SAVEPOINT.
    assert_eq!(
        reader
            .sql("SELECT count(*) AS n FROM accounts", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(4)
    );
    reader.sql("SAVEPOINT t", &[]).unwrap();
    reader
        .sql("UPDATE accounts SET balance = 1 WHERE id = 4", &[])
        .unwrap();
    reader.sql("ROLLBACK TO SAVEPOINT t", &[]).unwrap();
    reader.sql("RELEASE SAVEPOINT s", &[]).unwrap();
    reader.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        root.sql("SELECT balance FROM accounts WHERE id = 4", &[])
            .unwrap()
            .rows[0]["balance"],
        Value::Int(400)
    );
}

#[test]
fn typed_begin_and_catalog_mutations_honor_an_aborted_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("typed-aborted-catalog.db")).unwrap();
    seed_accounts(&root);
    root.sql("BEGIN", &[]).unwrap();
    root.sql("SELECT 1/0", &[]).unwrap_err();
    let error = root.begin().unwrap_err();
    assert_eq!(error.sqlstate(), Some("25P02"));
    assert!(root.create_graph("ghost_graph").is_err());
    assert!(root.create_sequence("ghost_sequence", 1, 1, false).is_err());
    root.rollback().unwrap();
    assert!(!root.has_graph("ghost_graph").unwrap());
    assert!(root.sequence_state("ghost_sequence").unwrap().is_none());
}

#[test]
fn foreign_key_insert_holds_key_share_on_the_parent_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("fk-key-share.db")).unwrap();
    root.sql("CREATE TABLE parents (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    root.sql(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id))",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO parents VALUES (1)", &[]).unwrap();
    let inserter = root.new_session().unwrap();
    let deleter = root.new_session().unwrap();
    inserter.sql("BEGIN", &[]).unwrap();
    inserter
        .sql("INSERT INTO children VALUES (10, 1)", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let delete_thread = std::thread::spawn(move || {
        done_tx
            .send(deleter.sql("DELETE FROM parents WHERE id = 1", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    inserter.sql("COMMIT", &[]).unwrap();
    let outcome = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    delete_thread.join().unwrap();
    assert!(
        outcome.is_err(),
        "the committed child must block the parent delete"
    );
    assert_eq!(
        root.sql("SELECT count(*) AS n FROM children", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
}

#[test]
fn on_conflict_do_nothing_waits_for_the_conflicting_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("do-nothing-waits.db")).unwrap();
    seed_accounts(&root);
    let deleter = root.new_session().unwrap();
    let inserter = root.new_session().unwrap();
    deleter.sql("BEGIN", &[]).unwrap();
    deleter
        .sql("DELETE FROM accounts WHERE id = 1", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let insert_thread = std::thread::spawn(move || {
        done_tx
            .send(inserter.sql(
                "INSERT INTO accounts (id, owner, balance) VALUES (1, 'new', 9) ON CONFLICT DO NOTHING",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    deleter.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    insert_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(
        root.sql("SELECT owner FROM accounts WHERE id = 1", &[])
            .unwrap()
            .rows[0]["owner"],
        Value::Str("new".into())
    );
}

#[test]
fn preselected_update_requalifies_rows_after_the_snapshot_advances() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("update-requalify.db")).unwrap();
    root.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, status TEXT, v INTEGER)",
        &[],
    )
    .unwrap();
    root.sql(
        "INSERT INTO t VALUES (1, 'active', 0), (2, 'active', 0)",
        &[],
    )
    .unwrap();
    let holder = root.new_session().unwrap();
    let updater = root.new_session().unwrap();
    // The holder pins row 1 so the updater's statement waits after having preselected both rows under its statement snapshot; row 2 changes meanwhile and must be requalified before it is rewritten.
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM t WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let update_thread = std::thread::spawn(move || {
        done_tx
            .send(updater.sql("UPDATE t SET v = 1 WHERE status = 'active'", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    holder
        .sql("UPDATE t SET status = 'done' WHERE id = 2", &[])
        .unwrap();
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    update_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    let rows = root
        .sql("SELECT id, status, v FROM t ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(rows[0].get("v"), Some(&Value::Int(1)));
    assert_eq!(rows[1].get("status"), Some(&Value::Str("done".into())));
    assert_eq!(rows[1].get("v"), Some(&Value::Int(0)));
}
