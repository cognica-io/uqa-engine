//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent role definitions retain private undo without rewriting unrelated roles.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::tests::relation_lock_support::{sessions, sql};
use uqa_sql::{ast::RoleAttribute, catalog::roles::RoleDefinition};

#[test]
fn independent_role_writers_commit_before_the_other_transaction_finishes() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE ROLE first_kept; CREATE ROLE first_removed; CREATE ROLE second_kept",
                );
                sql(
                    &first,
                    &format!(
                        "BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private"
                    ),
                );
                sql(&first, "CREATE ROLE first_created; ALTER ROLE first_kept LOGIN; DROP ROLE first_removed");
                sql(
                    &second,
                    "BEGIN; CREATE ROLE second_created; ALTER ROLE second_kept LOGIN",
                );
                assert!(!second
                    .storage
                    .catalog
                    .as_ref()
                    .unwrap()
                    .metadata_has_private_changes("sql_role_memberships_json")
                    .unwrap());
                sql(&second, "COMMIT");
                refresh_catalog(&first, isolation);
                {
                    let roles = first.durable.roles.read();
                    assert!(
                        roles.contains_key("first_created"),
                        "provider {provider}, {isolation}, {finish}"
                    );
                    assert!(roles.contains_key("second_created"));
                    assert!(roles["first_kept"].has(RoleAttribute::Login));
                    assert!(roles["second_kept"].has(RoleAttribute::Login));
                    assert!(!roles.contains_key("first_removed"));
                }
                let catalog = first.storage.catalog.as_ref().unwrap();
                assert!(!catalog
                    .metadata_has_private_changes("sql_roles_json")
                    .unwrap());
                assert!(catalog
                    .metadata_has_private_changes("sql_role_memberships_json")
                    .unwrap());
                sql(&first, finish);
                sql(&second, "SELECT * FROM pg_roles");
                let expected = second.durable.roles.snapshot();
                assert_eq!(expected.contains_key("first_created"), finish == "COMMIT");
                assert_eq!(
                    expected["first_kept"].has(RoleAttribute::Login),
                    finish == "COMMIT"
                );
                assert_eq!(expected.contains_key("first_removed"), finish != "COMMIT");
                assert!(expected.contains_key("second_created"));
                assert!(expected["second_kept"].has(RoleAttribute::Login));
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                assert_eq!(*reopened.durable.roles.read(), *expected);
            }
        }
    }
}

#[test]
fn private_role_attributes_refresh_over_an_external_role_deletion() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE kept; CREATE ROLE removed");
            sql(
                &first,
                &format!(
                    "BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; ALTER ROLE kept LOGIN"
                ),
            );
            sql(&second, "DROP ROLE removed");
            refresh_catalog(&first, isolation);
            assert!(first.durable.roles.read()["kept"].has(RoleAttribute::Login));
            assert!(!first.durable.roles.read().contains_key("removed"));
            sql(&first, "COMMIT");
            sql(&second, "SELECT * FROM pg_roles");
            assert!(second.durable.roles.read()["kept"].has(RoleAttribute::Login));
            assert!(!second.durable.roles.read().contains_key("removed"));
        }
    }
}

#[test]
fn role_oid_records_reject_two_stale_creators_of_the_same_oid() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "BEGIN");
        sql(&second, "BEGIN");
        for (engine, name) in [(&first, "first_role"), (&second, "second_role")] {
            engine.prepare_explicit_transaction_writer().unwrap();
            let before = engine.durable.roles.snapshot();
            let mut after = (*before).clone();
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_001;
            after.insert(name.into(), role);
            engine.persist_roles_snapshot(&before, &after).unwrap();
            *engine.durable.roles.write() = after;
            engine.note_catalog_registry_changed();
        }
        sql(&second, "COMMIT");
        assert_eq!(
            first.sql("COMMIT", &[]).unwrap_err().sqlstate(),
            Some("40001")
        );
        sql(&first, "ROLLBACK");
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        let roles = reopened.durable.roles.read();
        assert!(!roles.contains_key("first_role"));
        assert_eq!(roles["second_role"].oid, 20_001);
    }
}
