//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn captured_view_owners_admit_names_and_retain_the_selected_role_incarnation() {
    let mut role = RoleDefinition::bootstrap();
    role.name = "n".repeat(4 << 20);
    let identity = role.identity();
    let mut roles = BTreeMap::from([(role.name.clone(), role)]);
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("independent").unwrap();
    let before = budget.used();
    let occupied = budget.reserve(budget.available() - (32 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        let error = owner_reference_with_control(identity, &roles, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(observed.bytes_total < 1024, "{observed:?}");
    assert_eq!(budget.used(), before + occupied.bytes());
    drop(occupied);
    let owner = owner_reference_with_control(identity, &roles, &control).unwrap();
    let shared = owner.clone();
    assert!(budget.used() >= before + (4 << 20));
    let name = roles.first_key_value().unwrap().0.clone();
    let mut renamed = roles.remove(&name).unwrap();
    renamed.name = "renamed".into();
    roles.insert("renamed".into(), renamed);
    assert_eq!(owner.require_name(&roles).unwrap(), "renamed");
    drop(owner);
    assert!(budget.used() >= before + (4 << 20));
    drop(shared);
    assert_eq!(budget.used(), before);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn captured_view_owner_diagnostics_and_both_cancellation_scopes_are_preserved() {
    let role = RoleDefinition::bootstrap();
    let identity = role.identity();
    let roles = BTreeMap::from([(role.name.clone(), role)]);
    let budget = MemoryBudget::new(1 << 20);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            owner_reference_with_control(identity, &roles, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (identity, expected) in [
        (
            RoleIdentity {
                oid: 0,
                object_id: [0; 16],
            },
            "invalid persisted table role identity".to_owned(),
        ),
        (
            RoleIdentity {
                oid: identity.oid,
                object_id: [99; 16],
            },
            format!(
                "persisted table privileges reference missing role incarnation {}",
                identity.oid
            ),
        ),
    ] {
        let error = owner_reference_with_control(identity, &roles, &control).unwrap_err();
        let crate::SQLError::Internal(message) = error else {
            panic!("internal role error")
        };
        assert_eq!(message, expected);
        let ordinary = BoundTableSecurity::owner(identity)
            .owner_reference(&roles)
            .unwrap_err();
        let crate::SQLError::Internal(message) = ordinary else {
            panic!("ordinary role error")
        };
        assert_eq!(message, expected);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn captured_table_privileges_admit_only_acl_payloads_and_preserve_snapshots() {
    let roles = super::roles();
    let mut bound = super::security(&roles);
    let name = "c".repeat(4 << 20);
    let entries = bound.column_acls.remove("id").unwrap();
    bound.column_acls.insert(name.clone(), entries);
    bound.acl_revisions.columns.insert(name, [7; 16]);
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let occupied = budget.reserve(budget.available() - (32 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        assert_eq!(
            bound.capture_privileges(&control).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(observed.bytes_total < 4096, "{observed:?}");
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    let mut captured = None;
    let observed = allocation_counter::measure(|| {
        captured = Some(bound.capture_privileges(&control).unwrap());
    });
    let captured = captured.unwrap();
    assert_eq!(budget.used() as i64, observed.bytes_current);
    assert!(
        observed.bytes_total < (4 << 20) + (64 << 10),
        "{observed:?}"
    );
    let expected = bound.resolve(&roles).unwrap();
    assert_eq!(captured.resolve(&roles).unwrap(), expected);
    bound.acl = None;
    bound.column_acls.clear();
    assert_eq!(captured.resolve(&roles).unwrap(), expected);
    drop(captured);
    assert_eq!(budget.used(), 0);
}

#[test]
fn captured_table_privileges_release_partial_copies_and_observe_cancellation() {
    let roles = super::roles();
    let bound = super::security(&roles);
    let mut successes = 0;
    let mut failures = 0;
    for available in [0, 16, 64, 128, 256, 512, 1024, 4096] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("independent").unwrap();
        let before = budget.used();
        let occupied = budget.reserve(budget.available() - available).unwrap();
        match bound.capture_privileges(&control) {
            Ok(output) => {
                successes += 1;
                drop(output);
            }
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(budget.used(), before + occupied.bytes());
        drop(occupied);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    assert!(successes > 0 && failures > 1);
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            bound.capture_privileges(&control).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
