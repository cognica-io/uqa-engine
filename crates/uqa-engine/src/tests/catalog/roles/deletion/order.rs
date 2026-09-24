//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-target deletion observes earlier waits and transactional membership changes.

use super::*;
use crate::tests::catalog::roles::coordination::after_wait;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::shared_objects::SharedCatalogLock,
};

#[test]
fn role_deletion_resolves_later_targets_after_prior_waits_for_every_provider() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO keep; COMMIT"] {
                for later in ["CURRENT_USER", "absent", "IF_EXISTS"] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE selected");
                    let oid = u32::try_from(first.durable.roles.read()["selected"].oid).unwrap();
                    sql(&first, "BEGIN; SAVEPOINT keep; DROP ROLE selected");
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SAVEPOINT own"),
                    );
                    let command = if later == "IF_EXISTS" {
                        "DROP ROLE IF EXISTS selected, absent".into()
                    } else {
                        format!("DROP ROLE selected, {later}")
                    };
                    let (second, result) = after_wait(
                        &first,
                        second,
                        &command,
                        SharedCatalogLock::Object {
                            class_id: ROLE_CATALOG_CLASS_ID,
                            oid,
                        },
                        finish,
                    );
                    let state = match later {
                        "CURRENT_USER" => Some("22023"),
                        "absent" => Some("42704"),
                        _ if finish == "COMMIT" => Some("XX000"),
                        _ => None,
                    };
                    if let Some(state) = state {
                        assert_eq!(result.unwrap_err().sqlstate(), Some(state));
                    } else {
                        result.unwrap();
                    }
                    let expected_notices = if later == "IF_EXISTS" {
                        vec![(
                            "NOTICE".into(),
                            "role \"absent\" does not exist, skipping".into(),
                        )]
                    } else {
                        Vec::new()
                    };
                    assert_eq!(second.take_sql_notices(), expected_notices);
                    sql(&second, "ROLLBACK TO own; COMMIT");
                    for engine in [&first, &second] {
                        sql(engine, "SELECT rolname FROM pg_roles");
                        assert_eq!(
                            engine.durable.roles.read().contains_key("selected"),
                            finish != "COMMIT"
                        );
                    }
                    drop(second);
                    drop(first);
                    assert_eq!(
                        reopen(provider, &directory.path().join("table-locks.db"))
                            .durable
                            .roles
                            .read()
                            .contains_key("selected"),
                        finish != "COMMIT"
                    );
                }
            }
        }
    }
}

#[test]
fn role_deletion_membership_authority_follows_target_order_and_rolls_back_atomically() {
    for provider in 0..3 {
        for reverse in [false, true] {
            let (directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE actor CREATEROLE; CREATE ROLE first_target; CREATE ROLE second_target; GRANT first_target TO actor WITH ADMIN TRUE, INHERIT TRUE; GRANT second_target TO first_target WITH ADMIN TRUE");
            let roles = first.durable.roles.read().clone();
            let memberships = first.durable.role_memberships.read().clone();
            sql(&first, "BEGIN; SAVEPOINT keep; SET LOCAL ROLE actor");
            if reverse {
                sql(&first, "DROP ROLE second_target, first_target");
            } else {
                error(&first, "DROP ROLE first_target, second_target", "42501");
            }
            sql(&first, "ROLLBACK TO keep; COMMIT");
            for engine in [&first, &second] {
                sql(engine, "SELECT rolname FROM pg_roles");
                assert_eq!(*engine.durable.roles.read(), roles);
                assert_eq!(*engine.durable.role_memberships.read(), memberships);
            }
            sql(
                &first,
                "SET ROLE actor; DROP ROLE second_target, first_target; RESET ROLE",
            );
            drop(second);
            drop(first);
            let restored = reopen(provider, &directory.path().join("table-locks.db"));
            assert!(!restored.durable.roles.read().contains_key("first_target"));
            assert!(!restored.durable.roles.read().contains_key("second_target"));
            assert!(restored.durable.role_memberships.read().is_empty());
        }
    }
}

#[test]
fn role_deletion_keeps_initial_createrole_authority_through_object_waits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO keep; COMMIT"] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE actor CREATEROLE; CREATE ROLE first_target; CREATE ROLE second_target; CREATE ROLE helper; GRANT first_target, second_target TO actor WITH ADMIN TRUE");
                let oid = u32::try_from(first.durable.roles.read()["first_target"].oid).unwrap();
                sql(&first, "BEGIN; SAVEPOINT keep; GRANT first_target TO helper; ALTER ROLE actor NOCREATEROLE");
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SET LOCAL ROLE actor"),
                );
                let (second, result) = after_wait(
                    &first,
                    second,
                    "DROP ROLE first_target, second_target",
                    SharedCatalogLock::Object {
                        class_id: ROLE_CATALOG_CLASS_ID,
                        oid,
                    },
                    finish,
                );
                result.unwrap();
                sql(&second, "COMMIT");
                sql(&first, "SELECT rolname FROM pg_roles");
                assert!(!first.durable.roles.read().contains_key("first_target"));
                assert!(!first.durable.roles.read().contains_key("second_target"));
            }
        }
    }
}

#[test]
fn duplicate_role_deletion_rechecks_administration_and_dependency_errors_keep_target_order() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE ROLE actor CREATEROLE; CREATE ROLE duplicate_target; GRANT duplicate_target TO actor WITH ADMIN TRUE; SET ROLE actor");
        error(
            &first,
            "DROP ROLE duplicate_target, duplicate_target",
            "42501",
        );
        sql(
            &first,
            "RESET ROLE; DROP ROLE duplicate_target, duplicate_target",
        );
        sql(&first, "CREATE ROLE table_owner; CREATE ROLE schema_owner; CREATE TABLE owned(v INTEGER); ALTER TABLE owned OWNER TO table_owner; CREATE SCHEMA owned_schema AUTHORIZATION schema_owner");
        for (targets, expected) in [
            ("table_owner, schema_owner", "table_owner"),
            ("schema_owner, table_owner", "schema_owner"),
        ] {
            let error = first.sql(&format!("DROP ROLE {targets}"), &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("2BP01"));
            assert!(error
                .to_string()
                .contains(&format!("role \"{expected}\" cannot be dropped")));
            assert!(first.durable.roles.read().contains_key("table_owner"));
            assert!(first.durable.roles.read().contains_key("schema_owner"));
        }
    }
}

#[test]
fn role_definition_commands_use_transitive_administration_independent_of_inherit_and_set() {
    for provider in 0..3 {
        for inherit in [false, true] {
            for set in [false, true] {
                let (_directory, first, _second) = sessions(provider);
                sql(&first, &format!("CREATE ROLE actor CREATEROLE; CREATE ROLE middle; CREATE ROLE target; GRANT middle TO actor WITH INHERIT {}, SET {}; GRANT target TO middle WITH ADMIN TRUE; SET ROLE actor", if inherit { "TRUE" } else { "FALSE" }, if set { "TRUE" } else { "FALSE" }));
                sql(&first, "ALTER ROLE target LOGIN; ALTER ROLE target RENAME TO renamed_target; DROP ROLE renamed_target");
                assert!(!first.durable.roles.read().contains_key("target"));
                assert!(!first.durable.roles.read().contains_key("renamed_target"));
            }
        }
    }
}
