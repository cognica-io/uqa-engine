//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed catalog refresh preserves private role definitions and membership records.

use crate::tests::relation_lock_support::{sessions, sql};
use uqa_sql::ast::RoleAttribute;

const ISOLATIONS: &[&str] = &["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"];

pub(super) fn refresh_catalog(engine: &crate::Engine, isolation: &str) {
    engine.list_named_analyzers().unwrap();
    if isolation == "READ COMMITTED" {
        sql(
            engine,
            "SELECT * FROM pg_roles; SELECT * FROM pg_auth_members",
        );
    }
}

#[test]
fn private_role_definitions_and_memberships_survive_external_catalog_refresh() {
    for provider in 0..3 {
        for isolation in ISOLATIONS {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE kept; CREATE ROLE removed; CREATE ROLE member; CREATE ROLE other; GRANT kept TO member");
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&first, "CREATE ROLE private_role; ALTER ROLE kept LOGIN; DROP ROLE removed; REVOKE kept FROM member; GRANT private_role TO member");
            let roles = first.durable.roles.snapshot();
            let memberships = first.durable.role_memberships.snapshot();
            sql(&second, "GRANT MAINTAIN ON pg_class TO other");
            refresh_catalog(&first, isolation);
            assert_eq!(
                *first.durable.roles.read(),
                *roles,
                "provider {provider}, {isolation}"
            );
            assert_eq!(*first.durable.role_memberships.read(), *memberships);
            sql(&first, "ALTER ROLE private_role LOGIN; COMMIT");
            sql(
                &second,
                "SELECT * FROM pg_roles; SELECT * FROM pg_auth_members",
            );
            assert!(second.durable.roles.read()["private_role"].has(RoleAttribute::Login));
            assert!(second.durable.roles.read()["kept"].has(RoleAttribute::Login));
            assert!(!second.durable.roles.read().contains_key("removed"));
            assert_eq!(*second.durable.role_memberships.read(), *memberships);
        }
    }
}

#[test]
fn private_role_refresh_merges_only_the_metadata_record_changed_by_this_transaction() {
    for provider in 0..3 {
        for isolation in ISOLATIONS {
            for private_membership in [false, true] {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE ROLE kept; CREATE ROLE member; CREATE ROLE other",
                );
                sql(
                    &first,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                let (private, committed) = if private_membership {
                    ("GRANT kept TO member", "ALTER ROLE other LOGIN")
                } else {
                    ("ALTER ROLE other LOGIN", "GRANT kept TO member")
                };
                sql(&first, private);
                sql(&second, committed);
                refresh_catalog(&first, isolation);
                assert!(
                    first.durable.roles.read()["other"].has(RoleAttribute::Login),
                    "provider {provider}, {isolation}, membership {private_membership}"
                );
                assert!(first
                    .durable
                    .role_memberships
                    .read()
                    .values()
                    .any(|entry| entry.role.name == "kept" && entry.member.name == "member"));
                sql(&first, "COMMIT");
            }
        }
    }
}

#[test]
fn private_role_refresh_observes_savepoint_and_transaction_undo() {
    for provider in 0..3 {
        for isolation in ISOLATIONS {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE kept; CREATE ROLE member; CREATE ROLE other",
            );
            let initial = first.durable.roles.snapshot();
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&first, "CREATE ROLE outer_role; GRANT outer_role TO member; SAVEPOINT nested; CREATE ROLE inner_role; DROP ROLE kept; REVOKE outer_role FROM member; GRANT inner_role TO member");
            sql(&second, "GRANT MAINTAIN ON pg_class TO other");
            refresh_catalog(&first, isolation);
            assert!(first.durable.roles.read().contains_key("inner_role"));
            sql(&first, "ROLLBACK TO nested");
            sql(&second, "GRANT MAINTAIN ON pg_type TO other");
            refresh_catalog(&first, isolation);
            assert!(first.durable.roles.read().contains_key("outer_role"));
            assert!(first.durable.roles.read().contains_key("kept"));
            assert!(!first.durable.roles.read().contains_key("inner_role"));
            assert!(first
                .durable
                .role_memberships
                .read()
                .values()
                .any(|entry| entry.role.name == "outer_role"));
            assert!(!first
                .durable
                .role_memberships
                .read()
                .values()
                .any(|entry| entry.role.name == "inner_role"));
            sql(
                &first,
                "ROLLBACK; SELECT * FROM pg_roles; SELECT * FROM pg_auth_members",
            );
            assert_eq!(*first.durable.roles.read(), *initial);
            assert!(first.durable.role_memberships.read().is_empty());
        }
    }
}
