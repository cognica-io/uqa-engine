//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reject_roles(mut bytes: &[u8]) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    assert!(RoleDefinitions::read_image(&mut bytes, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn catalog_images_reject_every_truncation_and_trailing_byte() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&memory, &token, &token);
    let roles = catalog().capture(&control).unwrap();
    let schemas = schema_registry().capture(&control).unwrap();
    let database = DatabaseAuthority::bootstrap().capture(&control).unwrap();
    let mut memberships = RoleMemberships::new();
    let member = RoleMembership {
        oid: 999,
        role: RoleBinding::from_definition(&role("alpha", 42)).unwrap(),
        member: RoleBinding::from_definition(&role("omega", 99)).unwrap(),
        grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
        admin_option: true,
        inherit_option: true,
        set_option: false,
    };
    memberships.insert(member.key(), member).unwrap();
    let memberships = memberships.capture(&control).unwrap();
    let images = [
        streamed(roles.image_reader(&token).unwrap(), 7),
        streamed(memberships.image_reader(&token).unwrap(), 7),
        streamed(schemas.image_reader(&token).unwrap(), 7),
        streamed(database.image_reader(&token).unwrap(), 7),
    ];
    for (kind, bytes) in images.iter().enumerate() {
        for length in 0..=bytes.len() {
            let mut input = bytes[..length].to_vec();
            if length == bytes.len() {
                input.push(0);
            }
            let budget = MemoryBudget::new(1 << 20);
            let failed = match kind {
                0 => RoleDefinitions::read_image(&mut input.as_slice(), &budget, &token).is_err(),
                1 => RoleMemberships::read_image(&mut input.as_slice(), &budget, &token).is_err(),
                2 => SchemaRegistry::read_image(&mut input.as_slice(), &budget, &token).is_err(),
                _ => DatabaseAuthority::read_image(&mut input.as_slice(), &budget, &token).is_err(),
            };
            assert!(failed, "kind {kind}, length {length}");
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn role_image_rejects_invalid_identity_flags_keys_and_duplicates() {
    let mut roles = RoleDefinitions::new();
    roles.insert("a".into(), role("a", 42)).unwrap();
    let image = encode_roles(&roles);
    for (offset, bytes) in [
        (0, vec![0]),
        (24, vec![255]),
        (25, vec![0; 8]),
        (33, vec![0; 16]),
        (49, vec![0; 8]),
        (65, vec![b'b']),
        (66, vec![128]),
        (67, (-2_i32).to_le_bytes().to_vec()),
    ] {
        let mut invalid = image.clone();
        invalid[offset..offset + bytes.len()].copy_from_slice(&bytes);
        reject_roles(&invalid);
    }
    let mut duplicate = image.clone();
    duplicate[8..16].copy_from_slice(&2_u64.to_le_bytes());
    duplicate.extend_from_slice(&image[16..]);
    reject_roles(&duplicate);
    let mut reversed = catalog();
    reversed
        .insert("alpha".into(), role("different", 42))
        .unwrap();
    reject_roles(&encode_roles(&reversed));
    let mut duplicate_oid = catalog();
    duplicate_oid
        .insert("omega".into(), role("omega", 42))
        .unwrap();
    reject_roles(&encode_roles(&duplicate_oid));
    let mut duplicate_object = catalog();
    let mut value = role("omega", 99);
    value.object_id = role("alpha", 42).object_id;
    duplicate_object.insert("omega".into(), value).unwrap();
    reject_roles(&encode_roles(&duplicate_object));
}

#[test]
fn security_images_reject_unknown_privilege_bits_and_temporary_schemas() {
    let schemas = schema_registry();
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let captured = schemas
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let mut bytes = streamed(captured.image_reader(&token).unwrap(), 16);
    *bytes.last_mut().unwrap() = 128;
    let memory = MemoryBudget::new(1 << 20);
    assert!(SchemaRegistry::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    let mut singleton = SchemaRegistry::new();
    singleton
        .insert(
            "normal".into(),
            BoundSchemaSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    let captured = singleton
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let bytes = streamed(captured.image_reader(&token).unwrap(), 16);
    let mut invalid = bytes[..16].to_vec();
    invalid.extend_from_slice(&11_u64.to_le_bytes());
    invalid.extend_from_slice(b"pg_temp_123");
    invalid.extend_from_slice(&bytes[30..]);
    assert!(SchemaRegistry::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn membership_images_reject_key_mismatch_oid_and_option_corruption() {
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let member = RoleMembership {
        oid: 999,
        role: RoleBinding::from_definition(&role("alpha", 42)).unwrap(),
        member: RoleBinding::from_definition(&role("omega", 99)).unwrap(),
        grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
        admin_option: true,
        inherit_option: true,
        set_option: true,
    };
    let mut memberships = RoleMemberships::new();
    memberships.insert(member.key(), member).unwrap();
    let captured = memberships
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let image = streamed(captured.image_reader(&token).unwrap(), 16);
    for (offset, bytes) in [
        (16, 43_i64.to_le_bytes().to_vec()),
        (88, vec![0; 8]),
        (image.len() - 1, vec![8]),
    ] {
        let mut damaged = image.clone();
        damaged[offset..offset + bytes.len()].copy_from_slice(&bytes);
        let memory = MemoryBudget::new(1 << 20);
        assert!(RoleMemberships::read_image(&mut damaged.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn empty_catalog_images_and_single_database_count_are_canonical() {
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&source_memory, &token, &token);
    let roles = RoleDefinitions::new().capture(&control).unwrap();
    let memberships = RoleMemberships::new().capture(&control).unwrap();
    let schemas = SchemaRegistry::new().capture(&control).unwrap();
    for (magic, image) in [
        (
            b"UQAROL01",
            streamed(roles.image_reader(&token).unwrap(), 1),
        ),
        (
            b"UQARMB01",
            streamed(memberships.image_reader(&token).unwrap(), 1),
        ),
        (
            b"UQASCH01",
            streamed(schemas.image_reader(&token).unwrap(), 1),
        ),
    ] {
        assert_eq!(&image[..8], magic);
        assert_eq!(&image[8..], &[0; 8]);
    }
    let database = DatabaseAuthority::bootstrap().capture(&control).unwrap();
    let image = streamed(database.image_reader(&token).unwrap(), 16);
    for count in [0_u64, 2, u64::MAX] {
        let mut damaged = image.clone();
        damaged[8..16].copy_from_slice(&count.to_le_bytes());
        let memory = MemoryBudget::new(1 << 20);
        assert!(DatabaseAuthority::read_image(&mut damaged.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}
