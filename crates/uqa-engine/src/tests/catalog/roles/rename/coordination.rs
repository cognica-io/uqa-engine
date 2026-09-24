//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::catalog::roles::coordination::after_wait;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};

#[test]
fn renames_wait_for_the_selected_definition_tuple_and_preserve_undo_outcomes() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                for (holder, waiter, committed_name, undone_name) in [
                    (
                        "ALTER ROLE target LOGIN",
                        "ALTER ROLE target RENAME TO renamed",
                        Some("target"),
                        Some("renamed"),
                    ),
                    (
                        "ALTER ROLE target RENAME TO renamed",
                        "ALTER ROLE target LOGIN",
                        Some("renamed"),
                        Some("target"),
                    ),
                    (
                        "ALTER ROLE target RENAME TO renamed",
                        "DROP ROLE target",
                        Some("renamed"),
                        None,
                    ),
                    (
                        "DROP ROLE target",
                        "ALTER ROLE target RENAME TO renamed",
                        None,
                        Some("renamed"),
                    ),
                ] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE target");
                    let original = first.durable.roles.read()["target"].identity();
                    sql(&first, &format!("BEGIN; SAVEPOINT undo; {holder}"));
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        waiter,
                        SharedCatalogLock::Tuple {
                            class_id: ROLE_CATALOG_CLASS_ID,
                            oid: u32::try_from(original.oid).unwrap(),
                        },
                        finish,
                    );
                    if finish == "COMMIT" {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("XX000"),
                            "{provider}/{isolation}/{holder}/{waiter}"
                        );
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    sql(&first, "SELECT rolname FROM pg_roles");
                    let roles = first.durable.roles.read().clone();
                    let name = if finish == "COMMIT" {
                        committed_name
                    } else {
                        undone_name
                    };
                    assert_eq!(
                        roles
                            .values()
                            .find(|role| role.identity() == original)
                            .map(|role| role.name.as_str()),
                        name
                    );
                    drop(second);
                    drop(first);
                    let restored = reopen(provider, &directory.path().join("table-locks.db"));
                    assert_eq!(*restored.durable.roles.read(), roles);
                }
            }
        }
    }
}

#[test]
fn rename_destination_reservations_coordinate_creators_and_other_renamers() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                for (holder, waiter) in [
                    ("ALTER ROLE source RENAME TO taken", "CREATE ROLE taken"),
                    (
                        "ALTER ROLE source RENAME TO taken",
                        "ALTER ROLE target RENAME TO taken",
                    ),
                    ("CREATE ROLE taken", "ALTER ROLE target RENAME TO taken"),
                ] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE source; CREATE ROLE target");
                    sql(&first, &format!("BEGIN; SAVEPOINT undo; {holder}"));
                    let reserved = first.durable.roles.read()["taken"].identity();
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        waiter,
                        SharedCatalogLock::Name {
                            class_id: ROLE_CATALOG_CLASS_ID,
                            name: "taken",
                        },
                        finish,
                    );
                    if finish == "COMMIT" {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("23505"));
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    sql(&first, "SELECT rolname FROM pg_roles");
                    let roles = first.durable.roles.read().clone();
                    assert_eq!(roles["taken"].identity() == reserved, finish == "COMMIT");
                    drop(second);
                    drop(first);
                    assert_eq!(
                        *reopen(provider, &directory.path().join("table-locks.db"))
                            .durable
                            .roles
                            .read(),
                        roles
                    );
                }
            }
        }
    }
}

#[test]
fn membership_waits_retain_all_endpoints_when_their_names_change_and_are_reused() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE target; CREATE ROLE member; CREATE ROLE grantor; CREATE ROLE other; GRANT target TO grantor WITH ADMIN TRUE, INHERIT FALSE, SET FALSE");
                let originals = first.durable.roles.read().clone();
                sql(&first, "BEGIN; SAVEPOINT undo; GRANT target TO other");
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SET LOCAL ROLE grantor"),
                );
                let change = format!("ALTER ROLE target RENAME TO renamed_target; ALTER ROLE member RENAME TO renamed_member; ALTER ROLE grantor RENAME TO renamed_grantor; CREATE ROLE target; CREATE ROLE member; CREATE ROLE grantor; {finish}");
                let (second, result) = after_wait(
                    &first,
                    second,
                    "GRANT target TO member",
                    SharedCatalogLock::Object {
                        class_id: ROLE_CATALOG_CLASS_ID,
                        oid: u32::try_from(originals["target"].oid).unwrap(),
                    },
                    &change,
                );
                result.unwrap();
                sql(&second, "COMMIT");
                sql(&first, "SELECT rolname FROM pg_roles");
                let memberships = first.durable.role_memberships.read().clone();
                let retained = memberships
                    .values()
                    .find(|row| {
                        row.role.identity() == originals["target"].identity()
                            && row.member.identity() == originals["member"].identity()
                    })
                    .unwrap();
                assert_eq!(retained.grantor.identity(), originals["grantor"].identity());
                if finish == "COMMIT" {
                    assert_eq!(sql(&first, "SELECT pg_has_role('renamed_member', 'renamed_target', 'MEMBER') AS original, pg_has_role('member', 'target', 'MEMBER') AS replacement").rows[0], std::collections::BTreeMap::from([("original".into(), Value::Bool(true)), ("replacement".into(), Value::Bool(false))]));
                }
                drop(second);
                drop(first);
                assert_eq!(
                    *reopen(provider, &directory.path().join("table-locks.db"))
                        .durable
                        .role_memberships
                        .read(),
                    memberships
                );
            }
        }
    }
}
