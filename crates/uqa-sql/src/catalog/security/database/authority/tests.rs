//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    roles::RoleIdentity,
    security::database::{binding::BoundDatabaseAclEntry, DatabasePrivileges},
};

fn security(count: usize) -> BoundDatabaseSecurity {
    BoundDatabaseSecurity {
        role_owner: RoleIdentity::BOOTSTRAP,
        acl: Some(
            (0..count)
                .map(|index| BoundDatabaseAclEntry {
                    role: (!index.is_multiple_of(3)).then_some(RoleIdentity {
                        oid: 20000 + index as i64,
                        object_id: [7; 16],
                    }),
                    grantor: RoleIdentity::BOOTSTRAP,
                    privileges: DatabasePrivileges {
                        create: true,
                        connect: index.is_multiple_of(2),
                        temporary: index.is_multiple_of(3),
                    },
                    grant_options: DatabasePrivileges {
                        create: index.is_multiple_of(2),
                        ..DatabasePrivileges::default()
                    },
                })
                .collect(),
        ),
    }
}

#[test]
fn database_authority_capture_shares_exact_acl_generations_without_allocation() {
    for expected in [
        BoundDatabaseSecurity::bootstrap(),
        security(0),
        security(1),
        security(64),
        security(4096),
    ] {
        let native = MemoryBudget::new(8 << 20);
        let root = DatabaseAuthority::with_budget(&expected, &native).unwrap();
        let required = size_of::<Produced<BoundDatabaseSecurity>>()
            + expected.acl.as_ref().map_or(0, |entries| {
                entries.len() * size_of::<BoundDatabaseAclEntry>()
            });
        assert_eq!(native.used(), required);
        let memory = MemoryBudget::new(required);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(root.capture(&control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(memory.used(), required);
        let captured = captured.unwrap();
        assert!(std::ptr::eq(root.as_ref(), captured.as_ref()));
        let ordinary = expected
            .capture_security(&ProductionControl::uncontrolled())
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&*captured).unwrap(),
            serde_json::to_vec(&*ordinary).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&root).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let independent_memory = MemoryBudget::new(required);
        let independent = root
            .capture(&ProductionControl::new(&independent_memory, &token, &token))
            .unwrap();
        let changed = root.successor(&security(2)).unwrap();
        drop((root, changed));
        assert_eq!(captured.as_ref(), &expected);
        assert_eq!(independent.as_ref(), &expected);
        assert_eq!(native.used(), required);
        drop(captured);
        assert_eq!(memory.used(), 0);
        assert_eq!(native.used(), required);
        drop(independent);
        assert_eq!(independent_memory.used(), 0);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn database_authority_rejects_unfunded_successors_before_replacing_the_original() {
    let expected = security(64);
    let required =
        size_of::<Produced<BoundDatabaseSecurity>>() + 64 * size_of::<BoundDatabaseAclEntry>();
    let insufficient = MemoryBudget::new(required - 1);
    assert!(DatabaseAuthority::with_budget(&expected, &insufficient).is_err());
    assert_eq!(insufficient.used(), 0);
    let native = MemoryBudget::new(required);
    let root = DatabaseAuthority::with_budget(&expected, &native).unwrap();
    let unchanged = root.successor(&expected).unwrap();
    assert!(std::ptr::eq(root.as_ref(), unchanged.as_ref()));
    let mut changed = expected.clone();
    changed.acl.as_mut().unwrap()[0].grant_options.create = false;
    assert!(root.successor(&changed).is_err());
    assert_eq!(root.as_ref(), &expected);
    assert_eq!(native.used(), required);
    let token = CancellationToken::new();
    assert!(root
        .capture(&ProductionControl::new(&insufficient, &token, &token))
        .is_err());
    assert_eq!(insufficient.used(), 0);
    let capture = root
        .capture(&ProductionControl::new(
            &MemoryBudget::new(required),
            &token,
            &token,
        ))
        .unwrap();
    assert_eq!(capture.as_ref(), &expected);
    drop((root, unchanged, capture));
    assert_eq!(native.used(), 0);
}

#[test]
fn database_authority_capture_honors_both_cancellation_owners_without_changing_native_state() {
    let native = MemoryBudget::new(65536);
    let expected = security(64);
    let root = DatabaseAuthority::with_budget(&expected, &native).unwrap();
    let native_bytes = native.used();
    let memory = MemoryBudget::new(65536);
    assert!(root.capture(&ProductionControl::uncontrolled()).is_err());
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = root
            .capture(&ProductionControl::new(&memory, &original, &invoking))
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
        assert_eq!(native.used(), native_bytes);
        assert_eq!(root.as_ref(), &expected);
    }
    let token = CancellationToken::new();
    let capture = root
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drop((root, capture));
    assert_eq!(memory.used(), 0);
    assert_eq!(native.used(), 0);
}
