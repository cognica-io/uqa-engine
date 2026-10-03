//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row locks of nested transaction frames, which `Engine::begin` inside a transaction and PL/pgSQL exception blocks open.

use super::*;

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
    // A SQL BEGIN inside the block changes nothing; Engine::begin opens a nested frame.
    session.begin().unwrap();
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
    session.rollback().unwrap();
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
    session.begin().unwrap();
    session
        .sql("SELECT id FROM accounts WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    session.sql("SAVEPOINT s", &[]).unwrap();
    session.rollback().unwrap();
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
fn an_exception_block_that_catches_an_error_releases_only_its_own_row_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("exception-block-locks.db")).unwrap();
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
    session
        .sql(
            "DO $$ BEGIN PERFORM id FROM accounts WHERE id = 2 FOR UPDATE; RAISE EXCEPTION 'abandoned'; EXCEPTION WHEN raise_exception THEN NULL; END $$",
            &[],
        )
        .unwrap();
    assert_eq!(session.transaction_depth(), 1);
    // The block's subtransaction is rolled back with its lock; the transaction keeps its own.
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
