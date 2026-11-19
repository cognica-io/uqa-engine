//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleBinding, RoleMembership};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([10, 20, 30, 40].map(|oid| {
        let mut role = RoleDefinition::bootstrap();
        role.oid = oid;
        role.object_id = identity(oid).object_id;
        role.name = format!("role_{oid}");
        (role.name.clone(), role)
    }))
    .unwrap()
}

fn identity(oid: i64) -> RoleIdentity {
    if oid == 10 {
        return RoleIdentity::BOOTSTRAP;
    }
    RoleIdentity {
        oid,
        object_id: [u8::try_from(oid).unwrap(); 16],
    }
}

fn grant(role: i64, grantor: i64, option: bool) -> ObjectAclEntry {
    ObjectAclEntry {
        role: Some(identity(role)),
        grantor: identity(grantor),
        grant_option: option,
    }
}

fn membership(role: i64, member: i64, grantor: i64, oid: i64) -> RoleMembership {
    RoleMembership {
        oid,
        role: RoleBinding::new(
            format!("role_{role}"),
            u32::try_from(role).unwrap(),
            identity(role).object_id,
        ),
        member: RoleBinding::new(
            format!("role_{member}"),
            u32::try_from(member).unwrap(),
            identity(member).object_id,
        ),
        grantor: RoleBinding::new(
            format!("role_{grantor}"),
            u32::try_from(grantor).unwrap(),
            identity(grantor).object_id,
        ),
        admin_option: false,
        inherit_option: true,
        set_option: true,
    }
}

#[test]
fn native_authority_rejects_missing_and_replaced_incarnations_without_resolving_names() {
    let roles = roles();
    let memory = MemoryBudget::new(8192);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    catalog
        .object(
            identity(10),
            Some(&[grant(20, 10, true), grant(30, 20, false)]),
        )
        .unwrap();
    for missing in [
        identity(50),
        RoleIdentity {
            oid: 20,
            object_id: [99; 16],
        },
    ] {
        assert!(catalog.role(missing).is_err());
        let mut acl = grant(20, 10, false);
        acl.role = Some(missing);
        assert!(catalog.object(identity(10), Some(&[acl])).is_err());
        let mut acl = grant(20, 10, false);
        acl.grantor = missing;
        assert!(catalog.object(identity(10), Some(&[acl])).is_err());
    }
    drop(catalog);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_grant_graph_matches_existing_acl_validation_for_all_three_role_graphs() {
    let roles = roles();
    let memory = MemoryBudget::new(64 << 10);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    let edges = [(10, 20), (10, 30), (20, 10), (20, 30), (30, 10), (30, 20)];
    for selected in 0..64 {
        let acl = edges
            .iter()
            .enumerate()
            .filter(|(index, _)| selected & (1 << index) != 0)
            .map(|(_, (grantor, role))| grant(*role, *grantor, true))
            .collect::<Vec<_>>();
        let reference = super::super::object_acl::validate_acl(identity(10), Some(&acl), "fixture");
        assert_eq!(
            catalog.object(identity(10), Some(&acl)).is_ok(),
            reference.is_ok(),
            "{selected}"
        );
    }
}

#[test]
fn native_table_authority_combines_relation_options_with_column_grants() {
    let roles = roles();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    let select = TablePrivileges {
        select: true,
        ..TablePrivileges::default()
    };
    let mut table = BoundTableSecurity::owner(identity(10));
    table.acl = Some(vec![BoundAclEntry {
        role: Some(identity(20)),
        grantor: identity(10),
        privileges: select,
        grant_options: select,
    }]);
    table.column_acls.insert(
        "visible".into(),
        vec![BoundAclEntry {
            role: Some(identity(30)),
            grantor: identity(20),
            privileges: select,
            grant_options: TablePrivileges::default(),
        }],
    );
    catalog.table(&table, |name| Ok(name == "visible")).unwrap();
    assert!(catalog.table(&table, |_| Ok(false)).is_err());
    table.acl.as_mut().unwrap()[0].grant_options = TablePrivileges::default();
    assert!(catalog.table(&table, |_| Ok(true)).is_err());
    table.acl.as_mut().unwrap()[0].grant_options = select;
    table.column_acls.get_mut("visible").unwrap()[0]
        .privileges
        .delete = true;
    assert!(catalog.table(&table, |_| Ok(true)).is_err());
}

#[test]
fn native_authority_rejects_unrooted_duplicate_and_public_grant_options() {
    let roles = roles();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    for acl in [
        vec![grant(20, 30, true), grant(30, 20, true)],
        vec![grant(20, 10, false), grant(20, 10, true)],
        vec![ObjectAclEntry {
            role: None,
            grantor: identity(10),
            grant_option: true,
        }],
    ] {
        assert!(catalog.object(identity(10), Some(&acl)).is_err());
    }
    catalog.object(identity(10), None).unwrap();
    catalog.object(identity(10), Some(&[])).unwrap();
    catalog
        .object(
            identity(10),
            Some(&[ObjectAclEntry {
                role: None,
                grantor: identity(10),
                grant_option: false,
            }]),
        )
        .unwrap();
}

#[test]
fn native_membership_validation_preserves_parallel_grantors_and_rejects_cycles() {
    let roles = roles();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    let mut memberships = RoleMemberships::default();
    for edge in [
        membership(20, 30, 10, 100),
        membership(20, 30, 40, 101),
        membership(30, 40, 10, 102),
    ] {
        memberships.insert(edge.key(), edge).unwrap();
    }
    catalog.memberships(&memberships).unwrap();
    let cycle = membership(40, 20, 10, 103);
    memberships.insert(cycle.key(), cycle).unwrap();
    assert!(catalog.memberships(&memberships).is_err());
}

#[test]
fn native_membership_validation_preserves_deleted_endpoints_without_name_or_oid_rebinding() {
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let edge = membership(20, 30, 10, 100);
    let memberships = RoleMemberships::try_from_iter([(edge.key(), edge.clone())]).unwrap();
    let mut roles = roles();
    roles.remove("role_20").unwrap();
    roles.remove("role_30").unwrap();
    AuthorityCatalog::new(&roles, &memory, &cancel)
        .unwrap()
        .memberships(&memberships)
        .unwrap();
    for oid in [20, 30] {
        let mut replacement = RoleDefinition::bootstrap();
        replacement.name = format!("role_{oid}");
        replacement.oid = oid;
        replacement.object_id = [u8::try_from(oid + 1).unwrap(); 16];
        roles.insert(replacement.name.clone(), replacement).unwrap();
    }
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    catalog.memberships(&memberships).unwrap();
    assert!(catalog.role(edge.role.identity()).is_err());
    assert!(catalog.role(edge.member.identity()).is_err());
    let mut missing_grantor = edge.clone();
    missing_grantor.grantor = edge.member;
    let corrupt =
        RoleMemberships::try_from_iter([(missing_grantor.key(), missing_grantor)]).unwrap();
    assert!(catalog.memberships(&corrupt).is_err());
    drop(catalog);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_schema_grants_cannot_borrow_a_different_privilege_option() {
    let roles = roles();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    let usage = SchemaPrivileges {
        usage: true,
        create: false,
    };
    let create = SchemaPrivileges {
        usage: false,
        create: true,
    };
    let mut schema = BoundSchemaSecurity::owner(identity(10));
    schema.acl = Some(vec![
        BoundAclEntry {
            role: Some(identity(20)),
            grantor: identity(10),
            privileges: usage,
            grant_options: usage,
        },
        BoundAclEntry {
            role: Some(identity(30)),
            grantor: identity(20),
            privileges: create,
            grant_options: SchemaPrivileges::default(),
        },
    ]);
    assert!(catalog.schema(&schema).is_err());
    schema.acl.as_mut().unwrap()[1].privileges = usage;
    catalog.schema(&schema).unwrap();
}

#[test]
fn native_authority_validation_retires_admitted_workspace_on_quota_and_cancellation() {
    let roles = roles();
    let cancel = CancellationToken::new();
    let acl = [
        grant(20, 10, true),
        grant(30, 20, true),
        grant(40, 30, false),
    ];
    let mut quota_failures = 0;
    let mut successes = 0;
    for limit in (0..4096).step_by(32) {
        let memory = MemoryBudget::new(limit);
        let result = AuthorityCatalog::new(&roles, &memory, &cancel)
            .and_then(|catalog| catalog.object(identity(10), Some(&acl)));
        if result.is_ok() {
            successes += 1;
        } else {
            quota_failures += 1;
        }
        assert_eq!(memory.used(), 0, "limit {limit}");
    }
    assert!(quota_failures > 0 && successes > 0);
    let memory = MemoryBudget::new(8192);
    let catalog = AuthorityCatalog::new(&roles, &memory, &cancel).unwrap();
    cancel.cancel();
    assert!(catalog
        .object(identity(10), Some(&acl))
        .unwrap_err()
        .to_string()
        .contains("cancel"));
    drop(catalog);
    assert_eq!(memory.used(), 0);
}
