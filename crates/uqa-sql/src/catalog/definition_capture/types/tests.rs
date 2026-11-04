//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, Expr, ObjectAclEntry},
    catalog::{
        composite_type::{StoredComposite, StoredCompositeAttribute},
        domain::StoredDomain,
        enum_type::{StoredEnum, StoredEnumLabel},
        range_type::{StoredMultirange, StoredRange},
        shell_type::StoredShellType,
    },
};
use uqa_core::{
    catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken, EnumLabelKey,
    RelationIdentity, Value,
};

fn acl() -> Option<Vec<ObjectAclEntry>> {
    Some(vec![ObjectAclEntry {
        role: None,
        grantor: RoleIdentity {
            oid: 31_000,
            object_id: [8; 16],
        },
        grant_option: true,
    }])
}

fn domain() -> StoredDomain {
    let crate::Statement::CreateDomain(definition) = crate::compile("CREATE DOMAIN typed AS text COLLATE \"C\" DEFAULT 'retained' CONSTRAINT required NOT NULL CONSTRAINT checked CHECK(VALUE <> '')").unwrap().remove(0) else { panic!("domain") };
    StoredDomain {
        object_id: [1; 16],
        oid: 20_001,
        array_oid: Some(20_002),
        identity: RelationIdentity::new("Quoted.Schema", "Type.Name"),
        owner: RoleIdentity::BOOTSTRAP,
        definition,
        array_name: Some("_Type.Name".into()),
        usage_acl: acl(),
    }
}

fn enumeration() -> StoredEnum {
    StoredEnum {
        object_id: [2; 16],
        oid: 20_003,
        array_oid: 20_004,
        array_name: "_enm".into(),
        identity: RelationIdentity::new("Quoted.Schema", "enm"),
        owner: RoleIdentity::BOOTSTRAP,
        labels: vec![StoredEnumLabel {
            oid: 20_005,
            key: EnumLabelKey::from_bytes(vec![64, 0, 255, 1]).unwrap(),
            label: "한글🙂".into(),
            sort_order: f32::from_bits(0x3f80_0001),
        }],
        usage_acl: Some(vec![]),
    }
}

fn composite() -> StoredComposite {
    StoredComposite {
        object_id: [3; 16],
        oid: 20_006,
        relation_oid: 20_007,
        array_oid: 20_008,
        array_name: "_cmp".into(),
        identity: RelationIdentity::new("public", "cmp"),
        owner: RoleIdentity::BOOTSTRAP,
        attributes: vec![
            StoredCompositeAttribute {
                name: "removed".into(),
                ty: ColumnType::Integer,
                collation: None,
                collation_oid: Some(0),
                number: 1,
                dropped: true,
            },
            StoredCompositeAttribute {
                name: "remaining".into(),
                ty: ColumnType::Text,
                collation: Some("pg_catalog.C".into()),
                collation_oid: Some(950),
                number: 2,
                dropped: false,
            },
        ],
        usage_acl: None,
    }
}

fn range() -> StoredRange {
    StoredRange {
        object_id: [4; 16],
        oid: 20_009,
        array_oid: 20_010,
        array_name: "_rng".into(),
        identity: RelationIdentity::new("public", "rng"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: Some(20_011),
        subtype_difference_oid: Some(20_012),
        multirange: StoredMultirange {
            object_id: [5; 16],
            oid: 20_013,
            array_oid: 20_014,
            array_name: "_multi".into(),
            identity: RelationIdentity::new("Other.Schema", "multi"),
        },
        usage_acl: acl(),
    }
}

fn shell() -> StoredShellType {
    StoredShellType {
        object_id: [6; 16],
        oid: 20_015,
        identity: RelationIdentity::new("public", "pending"),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: None,
    }
}

#[test]
fn captured_types_keep_exact_identity_acl_order_keys_support_and_dropped_attributes() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    macro_rules! exact {
        ($capture:ident, $value:expr) => {{
            let source = BTreeMap::from([("Exact.Registry.Key".into(), $value)]);
            let expected = serde_json::to_value(source.iter().collect::<Vec<_>>()).unwrap();
            let mut captured = None;
            let measured = allocation_counter::measure(|| {
                captured = Some($capture(&source, &control).unwrap())
            });
            let captured = captured.unwrap();
            assert_eq!(
                measured.bytes_current,
                i64::try_from(memory.used()).unwrap()
            );
            assert_eq!(captured.reserved_bytes(), memory.used());
            drop(source);
            assert_eq!(serde_json::to_value(&*captured).unwrap(), expected);
            drop(captured);
            assert_eq!(memory.used(), 0);
        }};
    }
    exact!(domains, domain());
    exact!(enums, enumeration());
    exact!(composites, composite());
    exact!(ranges, range());
    exact!(shells, shell());
}

#[test]
fn captured_domain_constants_detach_source_owners_and_preserve_value_bits() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source_cancellation = CancellationToken::new();
    let source_control =
        ProductionControl::new(&source_memory, &source_cancellation, &source_cancellation);
    let expected = Value::Row(
        vec![
            Value::Str("x".repeat(8192)),
            Value::Float(f64::from_bits(0xfff8_0000_0000_0007)),
        ]
        .into(),
    );
    let value = crate::ir::RetainedConstantValue::from_produced(
        source_control.copy_value(&expected).unwrap(),
        &source_control,
    )
    .unwrap();
    let literal = crate::ir::RetainedScalarLiteral::with_control(
        value,
        None,
        None,
        Some(950),
        &source_control,
    )
    .unwrap();
    let mut source = domain();
    source.definition.default = Some(Expr::RetainedLiteral(Box::new(
        crate::ast::RetainedInputLiteral(literal),
    )));
    source_cancellation.cancel();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let captured = source.capture_definition(&control).unwrap();
    let Some(Expr::RetainedLiteral(saved)) = &captured.definition.default else {
        panic!("retained literal")
    };
    let Some(Expr::RetainedLiteral(original)) = &source.definition.default else {
        panic!("source literal")
    };
    assert!(!saved.0.value.shares_value_with(&original.0.value));
    drop(source);
    assert_eq!(source_memory.used(), 0);
    assert!(saved
        .0
        .value
        .resident_value()
        .unwrap()
        .has_same_representation(&expected));
    assert_eq!(saved.0.collation_oid, Some(950));
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn type_capture_denies_large_nested_payloads_and_releases_partial_admission() {
    let mut domain = domain();
    domain.definition.default = Some(Expr::Literal(Value::Str("x".repeat(32768))));
    let mut enumeration = enumeration();
    enumeration.labels[0].label = "x".repeat(32768);
    let mut composite = composite();
    composite.attributes[1].collation = Some("x".repeat(32768));
    let mut range = range();
    range.multirange.identity.schema = "x".repeat(32768);
    let mut shell = shell();
    shell.usage_acl = Some(vec![acl().unwrap().remove(0); 32768]);
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 1 << 24 } else { 4096 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let control = ProductionControl::new(&memory, &token, &token);
        macro_rules! denied {
            ($value:expr) => {{
                let error = $value.capture_definition(&control).unwrap_err();
                if cancelled {
                    assert!(matches!(error, SQLError::Cancelled(_)));
                } else {
                    assert_eq!(error.sqlstate(), Some("53200"));
                }
                assert_eq!(memory.used(), 0);
            }};
        }
        denied!(domain);
        denied!(enumeration);
        denied!(composite);
        denied!(range);
        denied!(shell);
    }
}
