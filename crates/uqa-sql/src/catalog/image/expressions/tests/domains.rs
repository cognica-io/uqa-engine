//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ConstraintCatalogIdentity, CreateDomain, DomainCheck, DomainNotNull, ObjectAclEntry},
    catalog::{domain::StoredDomain, type_definitions::TypeDefinitions},
};
use uqa_core::{catalog_role::RoleIdentity, memory::ProductionControl, RelationIdentity};

fn domain() -> StoredDomain {
    StoredDomain {
        object_id: [7; 16],
        oid: 65536,
        array_oid: Some(65537),
        identity: RelationIdentity::new("s", "d"),
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: "s.d".into(),
            base: ColumnType::Integer,
            collation: Some("old.C".into()),
            collation_oid: Some(950),
            default: Some(Expr::Literal(Value::Int(17))),
            not_null: Some(DomainNotNull {
                name: Some("nn".into()),
                catalog_identity: Some(ConstraintCatalogIdentity {
                    object_id: [8; 16],
                    oid: 65538,
                }),
            }),
            checks: vec![DomainCheck {
                name: Some("c".into()),
                catalog_identity: Some(ConstraintCatalogIdentity {
                    object_id: [9; 16],
                    oid: 65539,
                }),
                expression: Expr::Column("value".into()),
                validated: false,
            }],
        },
        array_name: Some("_d".into()),
        usage_acl: Some(vec![ObjectAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: true,
        }]),
    }
}

fn expected() -> Vec<u8> {
    let owner = RoleIdentity::BOOTSTRAP;
    let mut bytes = b"UQADOM01\x01\0\0\0\0\0\0\0".to_vec();
    bytes.extend(text("s.d"));
    bytes.extend([7; 16]);
    bytes.extend(65536u32.to_le_bytes());
    bytes.push(1);
    bytes.extend(65537u32.to_le_bytes());
    bytes.extend(text("s"));
    bytes.extend(text("d"));
    bytes.extend(owner.oid.to_le_bytes());
    bytes.extend(owner.object_id);
    bytes.push(1);
    bytes.extend(text("_d"));
    bytes.push(1);
    bytes.extend(1u64.to_le_bytes());
    bytes.push(0);
    bytes.extend(owner.oid.to_le_bytes());
    bytes.extend(owner.object_id);
    bytes.push(1);
    bytes.extend(text("s.d"));
    bytes.push(1);
    bytes.extend(text("old.C"));
    bytes.push(1);
    bytes.extend(950u32.to_le_bytes());
    bytes.extend([1, 1]);
    bytes.extend(1u64.to_le_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.extend(text("nn"));
    bytes.push(1);
    bytes.extend([8; 16]);
    bytes.extend(65538i64.to_le_bytes());
    bytes.push(6);
    bytes.extend(value_image(&Value::Int(17)));
    bytes.push(1);
    bytes.extend(text("c"));
    bytes.push(1);
    bytes.extend([9; 16]);
    bytes.extend(65539i64.to_le_bytes());
    bytes.extend([0, 3]);
    bytes.extend(text("value"));
    bytes
}

#[test]
fn captured_domain_images_match_native_wire_and_survive_source_retirement() {
    let mut domains = TypeDefinitions::<StoredDomain>::new();
    domains.insert("s.d".into(), domain()).unwrap();
    let mut temporary = domain();
    temporary.identity.schema = "pg_temp_17".into();
    temporary.definition.name = "pg_temp_17.d".into();
    domains.insert("pg_temp_17.d".into(), temporary).unwrap();
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let captured = domains
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    assert_eq!(captured.len(), 1);
    domains.clear();
    drop(domains);
    let retained = memory.used();
    for width in [1, 7, 4096, 20000] {
        let output = MemoryBudget::new(16 << 20);
        assert_eq!(
            collect(captured.image_reader(&output, &token).unwrap(), width),
            expected()
        );
        assert_eq!(output.used(), 0);
        assert_eq!(memory.used(), retained);
    }
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn domain_image_preparation_borrows_roots_and_legacy_absence_is_distinct() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let mut domains = TypeDefinitions::<StoredDomain>::new();
    let mut value = domain();
    value.array_oid = None;
    value.array_name = None;
    value.usage_acl = None;
    value.definition.collation = None;
    value.definition.collation_oid = None;
    value.definition.default = None;
    value.definition.not_null = None;
    value.definition.checks.clear();
    domains.insert("s.d".into(), value).unwrap();
    let captured = domains
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let zero = MemoryBudget::new(0);
    let allocations = allocation_counter::measure(|| {
        drop(captured.image_reader(&zero, &token).unwrap());
    });
    assert_eq!(allocations.count_total, 0);
    let bytes = collect(captured.image_reader(&memory, &token).unwrap(), 19);
    let mut expected = b"UQADOM01\x01\0\0\0\0\0\0\0".to_vec();
    expected.extend(text("s.d"));
    expected.extend([7; 16]);
    expected.extend(65536u32.to_le_bytes());
    expected.push(0);
    expected.extend(text("s"));
    expected.extend(text("d"));
    expected.extend(RoleIdentity::BOOTSTRAP.oid.to_le_bytes());
    expected.extend(RoleIdentity::BOOTSTRAP.object_id);
    expected.extend([0, 0]);
    expected.extend(text("s.d"));
    expected.extend([0, 0, 0, 0]);
    expected.extend(0u64.to_le_bytes());
    expected.push(1);
    assert_eq!(bytes, expected);
    drop(captured);
    assert_eq!(memory.used(), 0);
    let empty = TypeDefinitions::<StoredDomain>::new();
    let empty = empty
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    assert_eq!(
        collect(empty.image_reader(&zero, &token).unwrap(), 17),
        b"UQADOM01\0\0\0\0\0\0\0\0"
    );
    assert_eq!(zero.used(), 0);
}

#[test]
fn domain_images_keep_each_optional_field_and_constraint_validation_independent() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let mut images = std::collections::BTreeSet::new();
    for change in 0..16 {
        let mut value = domain();
        match change {
            0 => {}
            1 => value.array_oid = None,
            2 => value.array_name = None,
            3 => value.array_name = Some(String::new()),
            4 => value.usage_acl = None,
            5 => value.usage_acl = Some(vec![]),
            6 => value.definition.collation = None,
            7 => value.definition.collation_oid = None,
            8 => value.definition.collation_oid = Some(0),
            9 => value.definition.default = None,
            10 => value.definition.default = Some(Expr::Literal(Value::Null)),
            11 => value.definition.not_null = None,
            12 => value.definition.not_null.as_mut().unwrap().catalog_identity = None,
            13 => value.definition.checks[0].name = None,
            14 => value.definition.checks[0].catalog_identity = None,
            15 => value.definition.checks[0].validated = true,
            _ => unreachable!(),
        }
        let mut domains = TypeDefinitions::<StoredDomain>::new();
        domains.insert("s.d".into(), value).unwrap();
        let captured = domains
            .capture(&ProductionControl::new(&memory, &token, &token))
            .unwrap();
        let bytes = collect(captured.image_reader(&memory, &token).unwrap(), 29);
        assert!(images.insert(bytes), "lost native domain field {change}");
        drop(captured);
        assert_eq!(memory.used(), 0);
    }
}
