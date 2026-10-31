//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn borrowed_schema_acl_matches_name_resolution_and_validates_all_grantors_first() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let memberships = BTreeMap::new();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for mut security in [
        BoundSchemaSecurity::bootstrap("private"),
        BoundSchemaSecurity::with_public_privileges(true),
    ] {
        for subject in ["uqa", "missing"] {
            for privilege in [SchemaAclPrivilege::Usage, SchemaAclPrivilege::Create] {
                let ordinary = super::super::role_has_schema_privilege(
                    &security.resolve(&roles).unwrap(),
                    subject,
                    privilege,
                    &roles,
                    &memberships,
                );
                assert_eq!(
                    role_has_schema_privilege_with_control(
                        (&security).into(),
                        subject,
                        privilege,
                        &roles,
                        &memberships,
                        &control
                    )
                    .unwrap(),
                    ordinary
                );
            }
        }
        security
            .acl
            .get_or_insert_with(Vec::new)
            .push(BoundAclEntry {
                role: None,
                grantor: RoleIdentity {
                    oid: 123,
                    object_id: [123; 16],
                },
                privileges: SchemaPrivileges::ALL,
                grant_options: SchemaPrivileges::default(),
            });
        let ordinary = security.resolve(&roles).unwrap_err();
        for subject in ["uqa", "missing"] {
            let SQLError::Internal(message) = role_has_schema_privilege_with_control(
                (&security).into(),
                subject,
                SchemaAclPrivilege::Usage,
                &roles,
                &memberships,
                &control,
            )
            .unwrap_err() else {
                panic!("invalid ACL must preserve its internal diagnostic")
            };
            assert_eq!(message, ordinary);
        }
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn public_acl_scan_borrows_large_catalog_without_materializing_names() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let memberships = BTreeMap::new();
    let security = BoundSchemaSecurity {
        tuple: None,
        role_owner: RoleIdentity::BOOTSTRAP,
        acl: Some(vec![
            BoundAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: SchemaPrivileges::ALL,
                grant_options: SchemaPrivileges::default(),
            };
            4096
        ]),
    };
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        assert!(role_has_schema_privilege_with_control(
            (&security).into(),
            "missing",
            SchemaAclPrivilege::Usage,
            &roles,
            &memberships,
            &control
        )
        .unwrap());
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(budget.used(), 0);
}
