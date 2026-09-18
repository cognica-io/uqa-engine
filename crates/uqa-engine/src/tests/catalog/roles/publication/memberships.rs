//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership catalog overlays preserve each transaction's rows and undo boundary.

use super::{refresh_catalog, reopen, sessions, sql};
use std::collections::BTreeMap;
use uqa_sql::catalog::roles::{identity::RoleBinding, RoleMembership, RoleMembershipKey};

fn row(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
) -> Option<RoleMembership> {
    memberships
        .values()
        .find(|edge| edge.member.name == member)
        .cloned()
}

#[test]
fn independent_membership_writers_keep_private_creates_updates_deletes_and_peer_commits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE first_target; CREATE ROLE second_target; CREATE ROLE first_kept; CREATE ROLE first_created; CREATE ROLE first_removed; CREATE ROLE second_created; CREATE ROLE second_removed; GRANT first_target TO first_kept, first_removed; GRANT second_target TO second_removed");
                sql(
                    &first,
                    &format!(
                        "BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private"
                    ),
                );
                sql(&first, "GRANT first_target TO first_created; GRANT first_target TO first_kept WITH ADMIN TRUE; REVOKE first_target FROM first_removed");
                sql(&second, "BEGIN; GRANT second_target TO second_created WITH ADMIN TRUE; REVOKE second_target FROM second_removed; COMMIT");
                refresh_catalog(&first, isolation);
                {
                    let memberships = first.durable.role_memberships.read();
                    assert!(
                        row(&memberships, "first_created").is_some(),
                        "provider {provider}, {isolation}, {finish}"
                    );
                    assert!(row(&memberships, "first_kept").unwrap().admin_option);
                    assert!(row(&memberships, "first_removed").is_none());
                    assert!(row(&memberships, "second_created").unwrap().admin_option);
                    assert!(row(&memberships, "second_removed").is_none());
                    let catalog = first.storage.catalog.as_ref().unwrap();
                    assert!(!catalog
                        .metadata_has_private_changes("sql_role_memberships_json")
                        .unwrap());
                    for (key, json) in catalog
                        .metadata_with_prefix("uqa.sql.role_membership.v1:")
                        .unwrap()
                    {
                        let stored: RoleMembership = serde_json::from_str(&json).unwrap();
                        assert_eq!(
                            catalog.metadata_has_private_changes(&key).unwrap(),
                            stored.role.name == "first_target"
                        );
                    }
                }
                sql(&first, finish);
                sql(&second, "SELECT * FROM pg_auth_members");
                let expected = second.durable.role_memberships.snapshot();
                assert_eq!(
                    row(&expected, "first_created").is_some(),
                    finish == "COMMIT"
                );
                assert_eq!(
                    row(&expected, "first_kept").unwrap().admin_option,
                    finish == "COMMIT"
                );
                assert_eq!(
                    row(&expected, "first_removed").is_some(),
                    finish != "COMMIT"
                );
                assert!(row(&expected, "second_created").unwrap().admin_option);
                assert!(row(&expected, "second_removed").is_none());
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                assert_eq!(*reopened.durable.role_memberships.read(), *expected);
            }
        }
    }
}

#[test]
fn membership_oid_claims_reject_two_stale_publishers_of_the_same_oid() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE first_target; CREATE ROLE second_target; CREATE ROLE member",
        );
        sql(&first, "BEGIN");
        sql(&second, "BEGIN");
        for (engine, target) in [(&first, "first_target"), (&second, "second_target")] {
            engine.prepare_explicit_transaction_writer().unwrap();
            let roles = engine.durable.roles.snapshot();
            let before = engine.durable.role_memberships.snapshot();
            let mut after = (*before).clone();
            let membership = RoleMembership {
                oid: 30_001,
                role: RoleBinding::from_definition(&roles[target]).unwrap(),
                member: RoleBinding::from_definition(&roles["member"]).unwrap(),
                grantor: RoleBinding::from_definition(&roles["uqa"]).unwrap(),
                admin_option: false,
                inherit_option: true,
                set_option: true,
            };
            after.insert(membership.key(), membership);
            engine
                .persist_role_memberships_snapshot(&before, &after)
                .unwrap();
            *engine.durable.role_memberships.write() = after;
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
        let memberships = reopened.durable.role_memberships.read();
        assert_eq!(memberships.len(), 1);
        let membership = memberships.values().next().unwrap();
        assert_eq!(membership.role.name, "second_target");
        assert_eq!(membership.oid, 30_001);
    }
}
