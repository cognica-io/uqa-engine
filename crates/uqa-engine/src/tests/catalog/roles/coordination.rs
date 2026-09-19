//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role name and object reservations follow the session's transaction boundaries.

use crate::tests::relation_lock_support::{sessions, sql};
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};

pub(super) use crate::tests::relation_lock_support::after_shared_wait as after_wait;

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

#[test]
fn competing_role_drops_check_the_original_object_after_wait_even_with_if_exists() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for conditional in [false, true] {
                for finish in [
                    "COMMIT",
                    "ROLLBACK",
                    "ROLLBACK TO before_drop; COMMIT",
                    "RECREATE",
                ] {
                    let (_directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE dropped");
                    let oid = u32::try_from(first.durable.roles.read()["dropped"].oid).unwrap();
                    sql(&first, "BEGIN; SAVEPOINT before_drop; DROP ROLE dropped");
                    if finish == "RECREATE" {
                        sql(&first, "CREATE ROLE dropped");
                    }
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        if conditional {
                            "DROP ROLE IF EXISTS dropped"
                        } else {
                            "DROP ROLE dropped"
                        },
                        SharedCatalogLock::Object {
                            class_id: ROLE_CATALOG_CLASS_ID,
                            oid,
                        },
                        if finish == "RECREATE" {
                            "COMMIT"
                        } else {
                            finish
                        },
                    );
                    if finish == "COMMIT" || finish == "RECREATE" {
                        let error = result.unwrap_err();
                        assert_eq!(error.sqlstate(), Some("XX000"));
                        assert!(error
                            .to_string()
                            .contains(&format!("could not find tuple for role {oid}")));
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    let count = sql(
                        &first,
                        "SELECT count(*) AS n FROM pg_roles WHERE rolname = 'dropped'",
                    )
                    .rows[0]["n"]
                        .clone();
                    assert_eq!(count, uqa_core::Value::Int(i64::from(finish == "RECREATE")));
                }
            }
        }
    }
}
