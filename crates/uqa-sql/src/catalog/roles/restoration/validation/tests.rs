//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleBinding, RoleMembership};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn roles() -> RoleDefinitions {
    let mut roles = RoleDefinitions::initial_catalog();
    let mut role = RoleDefinition::bootstrap();
    role.name = "reader".into();
    role.oid = 41;
    role.object_id = [1; 16];
    roles.insert(role.name.clone(), role).unwrap();
    roles
}

fn membership() -> RoleMembership {
    RoleMembership {
        oid: 57,
        role: RoleBinding::new("removed_target".into(), 42, [2; 16]),
        member: RoleBinding::new("removed_member".into(), 43, [3; 16]),
        grantor: RoleBinding::new("reader_old_name".into(), 41, [1; 16]),
        admin_option: false,
        inherit_option: true,
        set_option: true,
    }
}

#[test]
fn role_validation_advances_one_original_definition_and_retires_scratch() {
    let roles = roles();
    for mode in [
        RoleValidationMode::Definitions,
        RoleValidationMode::Identities,
        RoleValidationMode::Revisions,
    ] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = begin_validation(
            &roles,
            mode,
            ProductionControl::new(&budget, &token, &token),
        );
        assert_eq!(budget.used(), 0);
        for _ in 0..roles.len() {
            assert!(!input.advance().unwrap());
        }
        assert!(input.advance().unwrap());
        assert_eq!(budget.used(), 0);
        token.cancel();
        assert!(input.advance().unwrap());
    }
}

fn role_error(roles: &RoleDefinitions, mode: RoleValidationMode) -> String {
    let mut input = begin_validation(roles, mode, ProductionControl::uncontrolled());
    loop {
        match input.advance() {
            Ok(false) => (),
            Ok(true) => panic!("invalid role catalog accepted"),
            Err(error) => {
                assert_eq!(
                    input.advance().unwrap_err().to_string(),
                    "role validation is finished"
                );
                return error.to_string();
            }
        }
    }
}

#[test]
fn role_validation_preserves_key_oid_incarnation_and_revision_error_order() {
    let mut values = roles();
    values
        .edit("reader", |role| {
            role.name = "wrong".into();
            role.oid = 0;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Definitions),
        "persisted role key `reader` does not match role name `wrong`"
    );
    values
        .edit("reader", |role| {
            role.name = "reader".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Definitions),
        "persisted role `reader` has an invalid OID 0"
    );
    values
        .edit("reader", |role| {
            role.oid = 10;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Definitions),
        "persisted role OID 10 is duplicated"
    );
    values
        .edit("reader", |role| {
            role.oid = 41;
            role.object_id = RoleIdentity::BOOTSTRAP.object_id;
            role.revision = 0;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Identities),
        "persisted role object identity is duplicated"
    );
    values
        .edit("uqa", |role| {
            role.object_id = [0; 16];
            Ok(())
        })
        .unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Identities),
        "persisted role `uqa` has no object identity"
    );
    values.remove("uqa").unwrap();
    assert_eq!(
        role_error(&values, RoleValidationMode::Identities),
        "persisted bootstrap role has an invalid object identity"
    );
    assert_eq!(
        role_error(&values, RoleValidationMode::Revisions),
        "persisted role `reader` has no tuple revision"
    );
}

#[test]
fn membership_validation_keeps_deleted_endpoints_and_resolves_renamed_grantors() {
    let roles = roles();
    let values = [membership()];
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let observed = std::cell::Cell::new(0);
    let entries = values.iter().inspect(|_| observed.set(observed.get() + 1));
    let mut input = begin_membership_validation(
        &roles,
        entries,
        ProductionControl::new(&budget, &token, &token),
    );
    for _ in 0..=roles.len() {
        assert!(!input.advance().unwrap());
        assert_eq!(observed.get(), 0);
    }
    assert!(!input.advance().unwrap());
    assert_eq!(observed.get(), 1);
    assert!(input.advance().unwrap());
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert!(input.advance().unwrap());
}

fn membership_error(values: &[RoleMembership]) -> String {
    let roles = roles();
    let mut input =
        begin_membership_validation(&roles, values.iter(), ProductionControl::uncontrolled());
    loop {
        match input.advance() {
            Ok(false) => (),
            Ok(true) => panic!("invalid membership catalog accepted"),
            Err(error) => return error.to_string(),
        }
    }
}

#[test]
fn membership_validation_preserves_bound_identity_grantor_oid_and_key_precedence() {
    let first = membership();
    let mut second = first.clone();
    second.role.oid = 0;
    second.grantor.object_id = [9; 16];
    assert_eq!(
        membership_error(&[first.clone(), second.clone()]),
        "persisted role membership has an invalid role identity"
    );
    second.role.oid = 42;
    assert_eq!(membership_error(&[first.clone(), second]), "persisted role membership `removed_member` -> `removed_target` has a missing grantor identity");
    let mut invalid_oid = first.clone();
    invalid_oid.oid = -1;
    assert_eq!(
        membership_error(&[invalid_oid]),
        "persisted role membership has an invalid OID -1"
    );
    assert_eq!(
        membership_error(&[first.clone(), first.clone()]),
        "persisted role membership OID 57 is duplicated"
    );
    let mut duplicate_key = first.clone();
    duplicate_key.oid = 58;
    assert_eq!(
        membership_error(&[first, duplicate_key]),
        "persisted role membership identity is duplicated"
    );
}

#[test]
fn role_validation_cancellation_retires_both_traversals_and_preserves_native_roots() {
    let roles = roles();
    let retained = roles.clone();
    let values = [membership()];
    for original_owner in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut definitions = begin_validation(&roles, RoleValidationMode::Identities, control);
        let mut memberships = begin_membership_validation(&roles, values.iter(), control);
        assert!(!definitions.advance().unwrap());
        assert!(!memberships.advance().unwrap());
        assert!(budget.used() > 0);
        if original_owner {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            definitions.advance(),
            Err(RoleRestorationError::Input(ValueRetentionError::Cancelled(
                _
            )))
        ));
        assert!(matches!(
            memberships.advance(),
            Err(RoleRestorationError::Input(ValueRetentionError::Cancelled(
                _
            )))
        ));
        assert_eq!(budget.used(), 0);
        assert!(definitions.advance().is_err());
        assert!(memberships.advance().is_err());
        assert!(roles.shares_roots(&retained));
    }
}

#[test]
fn membership_validation_accounts_for_every_live_node_and_retires_denied_prefixes() {
    let roles = roles();
    let values = [membership()];
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = begin_membership_validation(
        &roles,
        values.iter(),
        ProductionControl::new(&budget, &token, &token),
    );
    let allocation = allocation_counter::measure(|| {
        for _ in 0..=(roles.len() + values.len()) {
            assert!(!input.advance().unwrap());
        }
    });
    assert_eq!(
        usize::try_from(allocation.bytes_current).unwrap(),
        budget.used()
    );
    assert!(usize::try_from(allocation.bytes_max).unwrap() <= budget.peak());
    let peak = budget.peak();
    assert!(input.advance().unwrap());
    assert_eq!(budget.used(), 0);
    for limit in (0..peak).step_by(19).chain([peak - 1]) {
        let budget = MemoryBudget::new(limit);
        let mut input = begin_membership_validation(
            &roles,
            values.iter(),
            ProductionControl::new(&budget, &token, &token),
        );
        loop {
            match input.advance() {
                Ok(false) => (),
                Ok(true) => panic!("denied input completed"),
                Err(RoleRestorationError::Input(ValueRetentionError::Memory(_))) => break,
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
        assert_eq!(budget.used(), 0);
        assert!(budget.peak() <= limit);
    }
}

#[test]
fn membership_iterator_unwind_retires_original_scratch_and_terminates_input() {
    let roles = roles();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let entries =
        std::iter::from_fn(|| -> Option<&RoleMembership> { panic!("record observation") });
    let mut input = begin_membership_validation(
        &roles,
        entries,
        ProductionControl::new(&budget, &token, &token),
    );
    for _ in 0..=roles.len() {
        assert!(!input.advance().unwrap());
    }
    assert!(budget.used() > 0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.advance())).is_err());
    assert_eq!(budget.used(), 0);
    assert!(input.advance().is_err());
}
