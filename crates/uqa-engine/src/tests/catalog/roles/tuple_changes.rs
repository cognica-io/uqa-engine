//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition mutations retain the original role tuple across competing writes.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};

fn tuple(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Tuple {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()["changed"].oid).unwrap(),
    }
}

#[test]
fn role_attribute_mutations_wait_for_the_original_catalog_tuple() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                for setting in ["LOGIN", "NOLOGIN"] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE changed");
                    let target = tuple(&first);
                    let identity = first.durable.roles.read()["changed"].identity();
                    sql(
                        &first,
                        &format!("BEGIN; SAVEPOINT undo; ALTER ROLE changed {setting}"),
                    );
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        "ALTER ROLE changed CONNECTION LIMIT 3",
                        target,
                        finish,
                    );
                    if finish == "COMMIT" {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    let expected_limit = if finish == "COMMIT" { -1 } else { 3 };
                    let row = sql(
                        &first,
                        "SELECT rolcanlogin, rolconnlimit FROM pg_roles WHERE rolname = 'changed'",
                    );
                    assert_eq!(row.rows[0]["rolconnlimit"], Value::Int(expected_limit));
                    assert_eq!(
                        row.rows[0]["rolcanlogin"],
                        Value::Bool(finish == "COMMIT" && setting == "LOGIN")
                    );
                    drop(second);
                    drop(first);
                    let restored = reopen(provider, &directory.path().join("table-locks.db"));
                    let roles = restored.durable.roles.read();
                    assert_eq!(roles["changed"].identity(), identity);
                    assert_eq!(i64::from(roles["changed"].connection_limit), expected_limit);
                }
            }
        }
    }
}

#[test]
fn role_attribute_and_deletion_waits_preserve_commit_and_undo_outcomes() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                for holder_drops in [false, true] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE changed");
                    let target = tuple(&first);
                    let holder = if holder_drops {
                        "DROP ROLE changed"
                    } else {
                        "ALTER ROLE changed LOGIN"
                    };
                    let waiter = if holder_drops {
                        "ALTER ROLE changed LOGIN"
                    } else {
                        "DROP ROLE changed"
                    };
                    sql(&first, &format!("BEGIN; SAVEPOINT undo; {holder}"));
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) = after_wait(&first, second, waiter, target, finish);
                    if finish == "COMMIT" {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    let exists = holder_drops != (finish == "COMMIT");
                    assert_eq!(
                        sql(&first, "SELECT oid FROM pg_roles WHERE rolname = 'changed'")
                            .rows
                            .len(),
                        usize::from(exists)
                    );
                    drop(second);
                    drop(first);
                    let restored = reopen(provider, &directory.path().join("table-locks.db"));
                    assert_eq!(
                        restored.durable.roles.read().contains_key("changed"),
                        exists
                    );
                }
            }
        }
    }
}

#[test]
fn definition_updates_commit_while_a_membership_keeps_its_shared_role_dependency() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE changed; CREATE ROLE member");
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; GRANT changed TO member"),
            );
            let second = super::memberships::before_holder_ends(
                &first,
                second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; ALTER ROLE changed LOGIN; COMMIT"),
            );
            sql(&first, "COMMIT");
            assert_eq!(
                sql(
                    &first,
                    "SELECT rolcanlogin FROM pg_roles WHERE rolname = 'changed'"
                )
                .rows[0]["rolcanlogin"],
                Value::Bool(true)
            );
            assert_eq!(
                sql(
                    &first,
                    "SELECT pg_has_role('member', 'changed', 'MEMBER') AS allowed"
                )
                .rows[0]["allowed"],
                Value::Bool(true)
            );
            drop(second);
            drop(first);
            let restored = reopen(provider, &directory.path().join("table-locks.db"));
            assert_eq!(restored.durable.roles.read()["changed"].revision, 2);
            assert_eq!(
                sql(
                    &restored,
                    "SELECT pg_has_role('member', 'changed', 'MEMBER') AS allowed"
                )
                .rows[0]["allowed"],
                Value::Bool(true)
            );
        }
    }
}
