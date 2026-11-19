//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_catalog_input_keeps_original_roles_while_destination_mutations_are_independent() {
    let bytes = encode_roles(&catalog());
    let input = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut restored = RoleDefinitions::read_image_with_mutation_budget(
        &mut bytes.as_slice(),
        &input,
        &mutations,
        &token,
    )
    .unwrap();
    let selected = restored.clone();
    let original_name = selected["alpha"].name.as_ptr();
    let occupied = input.reserve(input.available()).unwrap();
    token.cancel();
    restored
        .edit("alpha", |value| {
            value.connection_limit = 7;
            Ok(())
        })
        .unwrap();
    restored.insert("new".into(), role("new", 120)).unwrap();
    assert!(restored.remove("omega").unwrap());
    assert_eq!(restored["alpha"].connection_limit, 7);
    assert_eq!(selected["alpha"].connection_limit, 13);
    assert_eq!(selected["alpha"].name.as_ptr(), original_name);
    let before = encode_roles(&restored);
    let denied = mutations.reserve(mutations.available()).unwrap();
    assert!(restored
        .insert("denied".into(), role("denied", 121))
        .is_err());
    assert_eq!(encode_roles(&restored), before);
    assert_eq!(encode_roles(&selected), bytes);
    drop((restored, denied));
    assert_eq!(mutations.used(), 0);
    assert!(input.used() > occupied.bytes());
    drop(selected);
    assert_eq!(input.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.used(), 0);
}

#[test]
fn native_database_input_retains_authority_until_its_last_reader_retires() {
    let original = BoundDatabaseSecurity {
        role_owner: RoleIdentity::BOOTSTRAP,
        acl: Some(vec![BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: DatabasePrivileges::ALL,
            grant_options: DatabasePrivileges::default(),
        }]),
    };
    let output = MemoryBudget::new(1 << 20);
    let output_token = CancellationToken::new();
    let control = ProductionControl::new(&output, &output_token, &output_token);
    let native = DatabaseAuthority::with_budget(&original, &MemoryBudget::new(1 << 20)).unwrap();
    let captured = native.capture(&control).unwrap();
    let bytes = streamed(captured.image_reader(&output_token).unwrap(), 7);
    drop((native, captured));
    let input = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let restored = DatabaseAuthority::read_image_with_mutation_budget(
        &mut bytes.as_slice(),
        &input,
        &mutations,
        &token,
    )
    .unwrap();
    let selected = restored.capture(&control).unwrap();
    let pointer = selected.acl.as_ref().unwrap().as_ptr();
    let occupied = input.reserve(input.available()).unwrap();
    token.cancel();
    let mut changed = original.clone();
    changed.acl.as_mut().unwrap()[0].privileges.create = false;
    let denied = mutations.reserve(mutations.available()).unwrap();
    assert!(restored.successor(&changed).is_err());
    assert_eq!(restored.as_ref(), &original);
    drop(denied);
    let successor = restored.successor(&changed).unwrap();
    assert_eq!(successor.as_ref(), &changed);
    drop((restored, successor));
    assert_eq!(mutations.used(), 0);
    assert_eq!(selected.acl.as_ref().unwrap().as_ptr(), pointer);
    assert_eq!(
        streamed(selected.image_reader(&output_token).unwrap(), 1),
        bytes
    );
    assert!(input.used() > occupied.bytes());
    drop(selected);
    assert_eq!(output.used(), 0);
    assert_eq!(input.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.used(), 0);
}
