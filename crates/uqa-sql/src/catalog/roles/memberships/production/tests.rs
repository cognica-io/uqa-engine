//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::identity::RoleBinding;
use crate::catalog::roles::RoleDefinition;
use crate::catalog::roles::RoleMembership;
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn graph(count: u8) -> (RoleDefinitions, RoleMemberships) {
    let roles = RoleDefinitions::try_from_iter((1..=count).map(|id| {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("r{id}");
        role.oid = i64::from(id) + 100;
        role.object_id = [id; 16];
        role.attributes.clear();
        (role.name.clone(), role)
    }))
    .unwrap();
    let memberships = RoleMemberships::try_from_iter((1..count).map(|id| {
        let role = RoleBinding::from_definition(&roles[&format!("r{}", id + 1)]).unwrap();
        let member = RoleBinding::from_definition(&roles[&format!("r{id}")]).unwrap();
        let edge = RoleMembership {
            oid: i64::from(id),
            role,
            member,
            grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
            admin_option: false,
            inherit_option: true,
            set_option: false,
        };
        (edge.key(), edge)
    }))
    .unwrap();
    (roles, memberships)
}

#[test]
fn controlled_inheritance_preserves_edges_identity_and_releases_traversal() {
    let (mut roles, mut memberships) = graph(8);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for disabled in [None, Some(4)] {
        if let Some(oid) = disabled {
            let key = *memberships
                .iter()
                .find(|(_, edge)| edge.oid == oid)
                .unwrap()
                .0;
            memberships
                .edit(&key, |edge| {
                    edge.inherit_option = false;
                    Ok(())
                })
                .unwrap()
                .unwrap();
        }
        for member in roles.values() {
            for target in roles.values() {
                let ordinary = crate::catalog::roles::role_inherits(
                    &roles,
                    &memberships,
                    &member.name,
                    &target.name,
                );
                assert_eq!(
                    role_inherits_with_control(
                        &roles,
                        &memberships,
                        &member.name,
                        &target.name,
                        &control
                    )
                    .unwrap(),
                    ordinary
                );
                assert_eq!(budget.used(), 0);
            }
        }
    }
    let bound = RoleBinding::from_definition(&roles["r1"]).unwrap();
    let mut renamed = roles.get("r1").unwrap().clone();
    roles.remove("r1").unwrap();
    renamed.name = "renamed".into();
    roles.insert(renamed.name.clone(), renamed).unwrap();
    assert!(role_inherits_with_control(&roles, &memberships, &bound, "r3", &control).unwrap());
    roles
        .edit("renamed", |value| {
            value.object_id = [99; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(!role_inherits_with_control(&roles, &memberships, &bound, "r3", &control).unwrap());
    assert_eq!(budget.used(), 0);
}

#[test]
fn membership_allowance_rejection_and_both_cancellation_owners_release_scratch() {
    let (roles, memberships) = graph(64);
    let budget = MemoryBudget::new(128);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(7).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error =
            role_inherits_with_control(&roles, &memberships, "r1", "r64", &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 7);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            role_inherits_with_control(&roles, &memberships, "r1", "r64", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 7);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
