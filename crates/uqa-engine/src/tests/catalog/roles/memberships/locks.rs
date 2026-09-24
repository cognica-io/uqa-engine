//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn different_targets_and_opposite_grants_commit_without_waiting_for_the_first_transaction() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE a; CREATE ROLE b");
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; GRANT a TO b"),
            );
            let second = before_holder_ends(
                &first,
                second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; GRANT b TO a; COMMIT"),
            );
            sql(&first, "COMMIT");
            let rows = membership_rows(&first);
            assert_eq!(rows.len(), 2);
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(membership_rows(&reopened), rows);
        }
    }
}

#[test]
fn role_creation_and_group_membership_use_the_same_target_lock_and_atomic_publication() {
    for provider in 0..3 {
        for statement in [
            "CREATE ROLE added IN ROLE target",
            "ALTER GROUP target ADD USER added",
        ] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE target; CREATE ROLE other");
            if statement.starts_with("ALTER") {
                sql(&first, "CREATE ROLE added");
            }
            sql(&first, "BEGIN; GRANT target TO other");
            let (second, result) = after_wait(
                &first,
                second,
                statement,
                role_lock(&first, "target"),
                "COMMIT",
            );
            result.unwrap();
            let rows = membership_rows(&first);
            assert_eq!(rows.len(), 2);
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(membership_rows(&reopened), rows);
        }
    }
}

#[test]
fn same_target_grants_wait_and_preserve_peer_tuples_across_commit_and_savepoint_undo() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE target; CREATE ROLE a; CREATE ROLE b");
            sql(&first, "BEGIN; SAVEPOINT undo; GRANT target TO a");
            sql(&second, "BEGIN; INSERT INTO t VALUES (2)");
            let (second, result) = after_wait(
                &first,
                second,
                "GRANT target TO b",
                role_lock(&first, "target"),
                finish,
            );
            result.unwrap();
            sql(&second, "COMMIT");
            let rows = membership_rows(&first);
            assert_eq!(rows.len(), if finish == "COMMIT" { 2 } else { 1 });
            drop(second);
            drop(first);
            let reopened = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(membership_rows(&reopened), rows);
            assert_eq!(sql(&reopened, "SELECT v FROM t").rows.len(), 2);
        }
    }
}
