//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::{
    table::{self, TableAclPrivilege},
    TablePrivileges,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn roles() -> BTreeMap<String, RoleDefinition> {
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 41;
    reader.object_id = [41; 16];
    reader.attributes.clear();
    [RoleDefinition::bootstrap(), reader]
        .into_iter()
        .map(|role| (role.name.clone(), role))
        .collect()
}

fn entry(role: Option<RoleIdentity>, select: bool) -> BoundTableAclEntry {
    BoundTableAclEntry {
        role,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges {
            select,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges::default(),
    }
}

#[test]
fn bound_selection_preserves_table_column_and_system_acl_decisions() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for subject in ["uqa", "reader", "missing"] {
        let subject = RoleReference::from(subject);
        let check = SelectPrivileges {
            subject: &subject,
            roles: &roles,
            memberships: &memberships,
            control,
        };
        for acl in [
            None,
            Some(vec![]),
            Some(vec![entry(None, true)]),
            Some(vec![entry(Some(roles["reader"].identity()), true)]),
            Some(vec![entry(None, false)]),
        ] {
            let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
            security.acl = acl;
            security
                .column_acls
                .insert("secret".into(), vec![entry(None, true)]);
            let names = security.resolve(&roles).unwrap();
            for column in [None, Some("id"), Some("secret")] {
                let expected = column.map_or_else(
                    || {
                        table::role_has_table_privilege(
                            &names,
                            &subject,
                            TableAclPrivilege::Select,
                            &roles,
                            &memberships,
                        )
                    },
                    |column| {
                        table::role_has_column_privilege(
                            &names,
                            column,
                            &subject,
                            TableAclPrivilege::Select,
                            &roles,
                            &memberships,
                        )
                    },
                );
                assert_eq!(check.relation(&security, column).unwrap(), expected);
                assert_eq!(budget.used(), 0);
            }
        }
        for relation in SystemRelation::all() {
            let names = relation.bootstrap_security().resolve(&roles).unwrap();
            let expected = table::role_has_table_privilege(
                &names,
                &subject,
                TableAclPrivilege::Select,
                &roles,
                &memberships,
            );
            assert_eq!(check.system(relation, None, None).unwrap(), expected);
        }
    }
}

#[test]
fn unselected_acl_bindings_are_validated_before_a_superuser_grant() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let subject = RoleReference::from("uqa");
    let check = SelectPrivileges {
        subject: &subject,
        roles: &roles,
        memberships: &memberships,
        control: ProductionControl::uncontrolled(),
    };
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let mut invalid = entry(None, true);
    invalid.grantor = RoleIdentity {
        oid: 999,
        object_id: [99; 16],
    };
    security.column_acls.insert("unread".into(), vec![invalid]);
    let expected = security.resolve(&roles).unwrap_err();
    let SQLError::Internal(actual) = check.relation(&security, None).unwrap_err() else {
        panic!("invalid persisted ACL");
    };
    assert_eq!(actual, expected);
}

#[test]
fn borrowed_acl_validation_does_not_copy_wide_column_metadata() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let subject = RoleReference::from("reader");
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(vec![entry(None, true)]);
    security
        .column_acls
        .insert("wide_column_".repeat(256 * 1024), vec![entry(None, false)]);
    let old = allocation_counter::measure(|| {
        drop(security.resolve(&roles).unwrap());
    });
    assert!(old.bytes_total > 2 * 1024 * 1024);
    let budget = MemoryBudget::new(37);
    let sibling = budget.reserve(37).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let check = SelectPrivileges {
        subject: &subject,
        roles: &roles,
        memberships: &memberships,
        control: ProductionControl::new(&budget, &original, &invoking),
    };
    check.control.check().unwrap();
    let measured = allocation_counter::measure(|| {
        assert!(check.relation(&security, None).unwrap());
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(budget.used(), 37);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            check.relation(&security, None).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 37);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn bound_relation_checks_preserve_every_table_and_column_privilege() {
    let roles = roles();
    let memberships = BTreeMap::new();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for granted in TableAclPrivilege::ALL {
        for subject in ["uqa", "reader", "missing"] {
            let check = RelationPrivileges {
                subject,
                roles: &roles,
                memberships: &memberships,
                control,
            };
            let grant = |role| BoundTableAclEntry {
                role,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: granted.mask(),
                grant_options: TablePrivileges::default(),
            };
            for acl in [
                None,
                Some(vec![]),
                Some(vec![grant(None)]),
                Some(vec![grant(Some(roles["reader"].identity()))]),
            ] {
                let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
                security.acl = acl;
                security
                    .column_acls
                    .insert("secret".into(), vec![grant(None)]);
                let names = security.resolve(&roles).unwrap();
                for requested in TableAclPrivilege::ALL {
                    for column in [None, Some("id"), Some("secret")] {
                        let expected = column.map_or_else(
                            || {
                                table::role_has_table_privilege(
                                    &names,
                                    subject,
                                    requested,
                                    &roles,
                                    &memberships,
                                )
                            },
                            |column| {
                                table::role_has_column_privilege(
                                    &names,
                                    column,
                                    subject,
                                    requested,
                                    &roles,
                                    &memberships,
                                )
                            },
                        );
                        assert_eq!(
                            check.relation_for(&security, column, requested).unwrap(),
                            expected,
                            "{subject}: {granted:?}/{requested:?}/{column:?}"
                        );
                        assert_eq!(budget.used(), 0);
                    }
                }
            }
        }
    }
}

mod columns;
