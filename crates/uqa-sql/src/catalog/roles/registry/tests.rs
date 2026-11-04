//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{capture, identity::RoleBinding};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn role(index: u32) -> RoleDefinition {
    let mut role = RoleDefinition::bootstrap();
    role.oid = i64::from(index) + 20_000;
    role.object_id = u128::from(index + 1).to_be_bytes();
    role.name = format!("role_{index:04}");
    role.connection_limit = i32::try_from(index).unwrap();
    role
}

fn membership(role: &RoleDefinition) -> RoleMembership {
    let bootstrap = RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap();
    RoleMembership {
        oid: role.oid + 100_000,
        role: RoleBinding::from_definition(role).unwrap(),
        member: bootstrap.clone(),
        grantor: bootstrap,
        admin_option: true,
        inherit_option: true,
        set_option: false,
    }
}

#[test]
fn role_and_membership_roots_capture_without_enumeration_or_allocation() {
    for count in [1, 64, 4096] {
        let mut roles = RoleDefinitions::new();
        let mut memberships = RoleMemberships::new();
        for index in 0..count {
            let role = role(index);
            let membership = membership(&role);
            memberships.insert(membership.key(), membership).unwrap();
            roles.insert(role.name.clone(), role).unwrap();
        }
        let expected_roles = roles.clone();
        let expected_memberships = memberships.clone();
        let budget = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some((
                roles.capture(&control).unwrap(),
                memberships.capture(&control).unwrap(),
            ));
        });
        assert_eq!(allocations.count_total, 0);
        let required = budget.used();
        let first = captured.take().unwrap();
        let exact = MemoryBudget::new(required);
        let independent = ProductionControl::new(&exact, &token, &token);
        let allocations = allocation_counter::measure(|| {
            captured = Some((
                roles.capture(&independent).unwrap(),
                memberships.capture(&independent).unwrap(),
            ));
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(exact.used(), required);
        roles.clear();
        memberships.clear();
        drop((roles, memberships));
        for (roles, memberships) in [&first, captured.as_ref().unwrap()] {
            assert!(roles.iter().eq(expected_roles.iter()));
            assert!(memberships.iter().eq(expected_memberships.iter()));
        }
        drop((first, captured));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

fn capture_with_legacy_comparison(
    roles: &RoleDefinitions,
    memberships: &RoleMemberships,
    control: &ProductionControl<'_>,
) -> (
    Produced<CapturedRoleDefinitions>,
    Produced<CapturedRoleMemberships>,
) {
    let legacy_roles: BTreeMap<_, _> = roles.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let legacy_memberships: BTreeMap<_, _> =
        memberships.iter().map(|(k, v)| (*k, v.clone())).collect();
    let json = serde_json::to_string(&legacy_roles).unwrap();
    assert_eq!(serde_json::to_string(&roles).unwrap(), json);
    assert_eq!(
        serde_json::from_str::<RoleDefinitions>(&json).unwrap(),
        *roles
    );
    let expected_roles = capture::roles(&legacy_roles, control).unwrap();
    let expected_memberships = capture::memberships(&legacy_memberships, control).unwrap();
    let saved_roles = roles.capture(control).unwrap();
    let saved_memberships = memberships.capture(control).unwrap();
    for ((name, saved), (old_name, old)) in saved_roles.iter().zip(expected_roles.iter()) {
        assert_eq!(name, old_name);
        assert_eq!(
            (
                saved.oid,
                saved.object_id,
                saved.revision,
                &saved.name,
                saved.connection_limit
            ),
            (
                old.oid,
                old.object_id,
                old.revision,
                &old.name,
                old.connection_limit
            )
        );
        assert!(saved.attributes.iter().eq(old.attributes.iter()));
    }
    assert!(saved_memberships
        .iter()
        .eq(expected_memberships.iter().map(|(k, v)| (k, v))));
    assert_eq!(
        serde_json::to_string(&*saved_memberships).unwrap(),
        serde_json::to_string(&*expected_memberships).unwrap()
    );
    (saved_roles, saved_memberships)
}

#[test]
fn selected_role_publication_shares_unrelated_payloads_and_preserves_legacy_values() {
    for count in [1, 32, 512] {
        let mut roles = RoleDefinitions::new();
        let mut memberships = RoleMemberships::new();
        let mut large = role(9000);
        large.name = "large".repeat(1 << 15);
        let large_name = large.name.clone();
        let large_membership = membership(&large);
        let large_key = large_membership.key();
        roles.insert(large_name.clone(), large).unwrap();
        memberships.insert(large_key, large_membership).unwrap();
        for index in 0..count {
            let role = role(index);
            let membership = membership(&role);
            memberships.insert(membership.key(), membership).unwrap();
            roles.insert(role.name.clone(), role).unwrap();
        }
        let original_roles = roles.clone();
        let original_memberships = memberships.clone();
        let selected = role(10_000);
        let selected_membership = membership(&selected);
        let key = selected_membership.key();
        let allocations = allocation_counter::measure(|| {
            roles
                .insert(selected.name.clone(), selected.clone())
                .unwrap();
            memberships
                .insert(key, selected_membership.clone())
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        for name in original_roles.keys() {
            assert!(roles.shares_definition(name, &original_roles));
        }
        for key in original_memberships.keys() {
            assert!(memberships.shares_definition(key, &original_memberships));
        }
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let (saved_roles, saved_memberships) =
            capture_with_legacy_comparison(&roles, &memberships, &control);
        roles
            .edit(&selected.name, |role| {
                role.connection_limit = -1;
                role.advance_revision()
            })
            .unwrap()
            .unwrap();
        memberships
            .edit(&key, |edge| {
                edge.admin_option = false;
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(saved_roles.get(&selected.name), Some(&selected));
        assert_eq!(saved_memberships.get(&key), Some(&selected_membership));
        assert_ne!(saved_roles.get(&selected.name), roles.get(&selected.name));
        assert_ne!(saved_memberships.get(&key), memberships.get(&key));
        assert!(roles.shares_definition(&large_name, &original_roles));
        assert!(memberships.shares_definition(&large_key, &original_memberships));
        drop((saved_roles, saved_memberships));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn denied_role_publication_and_independent_capture_preserve_original_roots() {
    let native = MemoryBudget::new(65536);
    let mut roles = RoleDefinitions::with_budget(&native);
    let mut memberships = RoleMemberships::with_budget(&native);
    for index in 0..7 {
        let role = role(index);
        let membership = membership(&role);
        memberships.insert(membership.key(), membership).unwrap();
        roles.insert(role.name.clone(), role).unwrap();
    }
    let original_roles = roles.clone();
    let original_memberships = memberships.clone();
    let blocked = native.reserve(native.available()).unwrap();
    let replacement = role(3);
    let edge = membership(&replacement);
    assert!(roles.insert(replacement.name.clone(), replacement).is_err());
    assert!(memberships.insert(edge.key(), edge).is_err());
    assert_eq!(roles, original_roles);
    assert_eq!(memberships, original_memberships);
    drop(blocked);
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let saved_roles = roles.capture(&control).unwrap();
    let role_bytes = budget.used();
    let saved_memberships = memberships.capture(&control).unwrap();
    let membership_bytes = budget.used() - role_bytes;
    let small_roles = MemoryBudget::new(role_bytes - 1);
    let small_memberships = MemoryBudget::new(membership_bytes - 1);
    assert!(roles
        .capture(&ProductionControl::new(&small_roles, &token, &token))
        .is_err());
    assert!(memberships
        .capture(&ProductionControl::new(&small_memberships, &token, &token))
        .is_err());
    assert_eq!(small_roles.used(), 0);
    assert_eq!(small_memberships.used(), 0);
    token.cancel();
    assert!(matches!(
        roles.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        memberships.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((roles, memberships, original_roles, original_memberships));
    assert!(native.used() > 0);
    drop((saved_roles, saved_memberships));
    assert_eq!(native.used(), 0);
    assert_eq!(budget.used(), 0);
}
