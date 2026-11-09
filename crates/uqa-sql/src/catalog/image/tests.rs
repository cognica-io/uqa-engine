//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RoleAttribute,
    catalog::{
        roles::{
            identity::{RoleBinding, RoleSubject},
            RoleDefinition, RoleIdentity, RoleMembership,
        },
        security::{
            database::{BoundDatabaseSecurity, DatabasePrivileges},
            BoundSchemaSecurity,
        },
    },
};
use uqa_core::{
    catalog_role::BoundAclEntry,
    catalog_schema::{SchemaPrivileges, SchemaTupleIdentity},
    memory::{OwnedSet, ProductionControl},
};

mod authorities;
mod functions;
mod metadata;
mod native;
mod retention;
mod sequences;
mod validation;

fn role(name: &str, oid: i64) -> RoleDefinition {
    RoleDefinition {
        oid,
        object_id: (oid as u128).to_le_bytes(),
        revision: 9,
        name: name.into(),
        attributes: OwnedSet::from([RoleAttribute::Superuser, RoleAttribute::Login]),
        connection_limit: 13,
    }
}
fn catalog() -> RoleDefinitions {
    let mut roles = RoleDefinitions::new();
    for value in [role("alpha", 42), role("omega", 99)] {
        roles.insert(value.name.clone(), value).unwrap();
    }
    roles
}
fn encode_roles(roles: &RoleDefinitions) -> Vec<u8> {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let captured = roles
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let mut bytes = vec![];
    captured.write_image(&mut bytes, &token).unwrap();
    bytes
}
fn streamed(mut reader: CatalogImageReader<'_>, width: usize) -> Vec<u8> {
    let mut result = vec![];
    let mut buffer = vec![0; width];
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        result.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    result
}

#[test]
fn role_catalog_image_matches_independent_wire_fixture() {
    let mut roles = RoleDefinitions::new();
    let value = RoleDefinition {
        object_id: [7; 16],
        ..role("a", 42)
    };
    roles.insert("a".into(), value).unwrap();
    // Literal version-one layout: count, native key, OID, incarnation, revision, name, attribute bits, connection limit.
    let mut expected = b"UQAROL01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0a\x2a\0\0\0\0\0\0\0".to_vec();
    expected.extend_from_slice(&[7; 16]);
    expected.extend_from_slice(b"\x09\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0a\x11\x0d\0\0\0");
    assert_eq!(encode_roles(&roles), expected);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let restored = RoleDefinitions::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert!(roles == restored);
    assert!(memory.used() > 0);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn membership_image_retains_original_bindings_and_all_option_bits() {
    let roles = catalog();
    let mut memberships = RoleMemberships::new();
    let membership = RoleMembership {
        oid: 1234,
        role: RoleBinding::new("before_rename".into(), 42, role("alpha", 42).object_id),
        member: RoleBinding::new("omega".into(), 99, role("omega", 99).object_id),
        grantor: RoleBinding::from_definition(&RoleDefinition::bootstrap()).unwrap(),
        admin_option: true,
        inherit_option: false,
        set_option: true,
    };
    let key = membership.key();
    memberships.insert(key, membership.clone()).unwrap();
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let captured = memberships
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    let expected = streamed(captured.image_reader(&token).unwrap(), 1);
    for width in [2, 7, 64, 4096, 20000] {
        assert_eq!(
            streamed(captured.image_reader(&token).unwrap(), width),
            expected
        );
    }
    let memory = MemoryBudget::new(1 << 20);
    let restored = RoleMemberships::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert_eq!(restored.get(&key).unwrap(), &membership);
    assert_eq!(restored[&key].role.role_name(&roles), Some("alpha"));
    assert_eq!(restored[&key].role.name, "before_rename");
    drop(restored);
    assert_eq!(memory.used(), 0);
}

fn schema_registry() -> SchemaRegistry {
    let mut schemas = SchemaRegistry::new();
    let owner = RoleIdentity::BOOTSTRAP;
    for (name, oid, acl) in [
        ("default", 101, None),
        ("empty", 102, Some(vec![])),
        (
            "paths",
            103,
            Some(vec![
                BoundAclEntry {
                    role: None,
                    grantor: owner,
                    privileges: SchemaPrivileges {
                        usage: true,
                        create: false,
                    },
                    grant_options: SchemaPrivileges::default(),
                },
                BoundAclEntry {
                    role: Some(role("alpha", 42).identity()),
                    grantor: owner,
                    privileges: SchemaPrivileges::ALL,
                    grant_options: SchemaPrivileges {
                        usage: false,
                        create: true,
                    },
                },
            ]),
        ),
    ] {
        schemas
            .insert(
                name.into(),
                BoundSchemaSecurity {
                    tuple: Some(SchemaTupleIdentity::initial(oid)),
                    role_owner: owner,
                    acl,
                },
            )
            .unwrap();
    }
    schemas
}

#[test]
fn schema_image_preserves_tuple_and_acl_option_order_and_excludes_temporary_names() {
    let mut schemas = schema_registry();
    let expected = schemas.clone();
    schemas
        .insert(
            "pg_temp_999".into(),
            BoundSchemaSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    let token = CancellationToken::new();
    let source_memory = MemoryBudget::new(1 << 20);
    let captured = schemas
        .capture(&ProductionControl::new(&source_memory, &token, &token))
        .unwrap();
    assert_eq!(captured.len(), 3);
    let bytes = streamed(captured.image_reader(&token).unwrap(), 1);
    let mut written = vec![];
    assert_eq!(
        captured.write_image(&mut written, &token).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(written, bytes);
    let memory = MemoryBudget::new(1 << 20);
    let restored = SchemaRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(restored == expected);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn database_image_preserves_default_empty_and_ordered_acl_paths() {
    let owner = RoleIdentity::BOOTSTRAP;
    let token = CancellationToken::new();
    for acl in [
        None,
        Some(vec![]),
        Some(vec![
            BoundAclEntry {
                role: None,
                grantor: owner,
                privileges: DatabasePrivileges {
                    connect: true,
                    create: false,
                    temporary: true,
                },
                grant_options: DatabasePrivileges::default(),
            },
            BoundAclEntry {
                role: Some(role("alpha", 42).identity()),
                grantor: owner,
                privileges: DatabasePrivileges::ALL,
                grant_options: DatabasePrivileges {
                    connect: false,
                    create: true,
                    temporary: false,
                },
            },
        ]),
    ] {
        let original = BoundDatabaseSecurity {
            role_owner: owner,
            acl,
        };
        let native =
            DatabaseAuthority::with_budget(&original, &MemoryBudget::new(1 << 20)).unwrap();
        let source_memory = MemoryBudget::new(1 << 20);
        let captured = native
            .capture(&ProductionControl::new(&source_memory, &token, &token))
            .unwrap();
        let bytes = streamed(captured.image_reader(&token).unwrap(), 1);
        let memory = MemoryBudget::new(1 << 20);
        let restored =
            DatabaseAuthority::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
        assert_eq!(*restored, original);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn role_image_preserves_every_attribute_bit() {
    let mut value = RoleDefinition::bootstrap();
    value.attributes.insert(RoleAttribute::Replication);
    let mut roles = RoleDefinitions::new();
    roles.insert(value.name.clone(), value.clone()).unwrap();
    let bytes = encode_roles(&roles);
    assert_eq!(bytes[bytes.len() - 5], 127);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let restored = RoleDefinitions::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(restored.get("uqa").unwrap(), &value);
    assert_eq!(restored["uqa"].attributes.len(), 7);
    drop(restored);
    assert_eq!(memory.used(), 0);
}
