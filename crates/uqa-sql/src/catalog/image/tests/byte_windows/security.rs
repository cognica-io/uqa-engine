//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn membership_input_bounds_native_bindings_and_retires_original_fields() {
    let membership = RoleMembership {
        oid: 1234,
        role: RoleBinding::new("한🙂".repeat(1025), 42, role("alpha", 42).object_id),
        member: RoleBinding::new("omega".into(), 99, role("omega", 99).object_id),
        grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
        admin_option: true,
        inherit_option: false,
        set_option: true,
    };
    let key = membership.key();
    let mut source = RoleMemberships::new();
    source.insert(key, membership.clone()).unwrap();
    let source_memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let saved = source
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let bytes = streamed(saved.image_reader(&token).unwrap(), 7);
    drop((saved, source));
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            RoleMemberships::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(restored[&key], membership);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, RoleMemberships::image_input);
}

#[test]
fn database_authority_input_bounds_native_acl_and_retires_original_fields() {
    let expected = BoundDatabaseSecurity {
        role_owner: RoleIdentity::BOOTSTRAP,
        acl: Some(vec![
            BoundAclEntry {
                role: Some(role("alpha", 42).identity()),
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: DatabasePrivileges::ALL,
                grant_options: DatabasePrivileges::default(),
            };
            257
        ]),
    };
    let source_memory = MemoryBudget::new(1 << 20);
    let source = DatabaseAuthority::with_budget(&expected, &source_memory).unwrap();
    let token = CancellationToken::new();
    let saved = source
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let bytes = streamed(saved.image_reader(&token).unwrap(), 7);
    drop((saved, source));
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            DatabaseAuthority::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(*restored, expected);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, DatabaseAuthority::image_input);
}

#[test]
fn synchronous_empty_catalog_input_needs_no_continuation_allocation() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        let roles = RoleDefinitions::read_image(
            &mut b"UQAROL01\0\0\0\0\0\0\0\0".as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        let memberships = RoleMemberships::read_image(
            &mut b"UQARMB01\0\0\0\0\0\0\0\0".as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        let sequences = SequenceSecurities::read_image(
            &mut b"UQASQA01\0\0\0\0\0\0\0\0".as_slice(),
            &memory,
            &token,
        )
        .unwrap();
        assert!(roles.is_empty() && memberships.is_empty() && sequences.is_empty());
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(memory.used(), 0);
}
