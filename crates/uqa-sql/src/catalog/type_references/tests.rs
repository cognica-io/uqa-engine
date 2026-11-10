//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CreateDomain, EnumTypeReference};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

#[derive(Default)]
struct Types {
    domains: TypeDefinitions<StoredDomain>,
    enums: TypeDefinitions<StoredEnum>,
    composites: TypeDefinitions<StoredComposite>,
    ranges: TypeDefinitions<StoredRange>,
    shells: TypeDefinitions<StoredShellType>,
}

impl Types {
    fn input(&self) -> TypeReferenceInput<'_> {
        TypeReferenceInput {
            domains: &self.domains,
            enums: &self.enums,
            composites: &self.composites,
            ranges: &self.ranges,
            shells: &self.shells,
        }
    }
    fn with_enum() -> Self {
        let mut result = Self::default();
        result
            .enums
            .insert(
                "public.mood".into(),
                StoredEnum {
                    object_id: [1; 16],
                    oid: 20_000,
                    array_oid: 20_001,
                    array_name: "_mood".into(),
                    identity: RelationIdentity::new("public", "mood"),
                    owner: RoleIdentity::BOOTSTRAP,
                    labels: Vec::new(),
                    usage_acl: None,
                },
            )
            .unwrap();
        result
    }
}

fn mood() -> ColumnType {
    ColumnType::Enum(EnumTypeReference {
        schema: "old_schema".into(),
        name: "old_name".into(),
        oid: 20_000,
        array_oid: 20_001,
    })
}

#[test]
fn native_type_references_follow_identity_and_reject_wrong_kind_or_array_metadata() {
    let types = Types::with_enum();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    index
        .column_type(&ColumnType::Array(Box::new(mood())))
        .unwrap();
    index.stored_name("enum#20000[][]").unwrap();
    for name in ["domain#20000", "enum#20001", "enum#20002", "enum#bad"] {
        assert!(index.stored_name(name).is_err(), "{name}");
    }
    let mut wrong_array = mood();
    let ColumnType::Enum(reference) = &mut wrong_array else {
        unreachable!()
    };
    reference.array_oid += 1;
    assert!(index.column_type(&wrong_array).is_err());
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_type_claims_reject_cross_family_array_and_relation_collisions() {
    let mut types = Types::with_enum();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    for oid in [20_000, 20_001, 23, 1007, 2275, 2279, 13_306, 13_307] {
        types
            .shells
            .insert(
                "public.other".into(),
                StoredShellType {
                    object_id: [2; 16],
                    oid,
                    identity: RelationIdentity::new("public", "other"),
                    owner: RoleIdentity::BOOTSTRAP,
                    usage_acl: None,
                },
            )
            .unwrap();
        assert!(TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).is_err());
        assert_eq!(memory.used(), 0);
    }
    types.shells.remove("public.other").unwrap();
    let row = RelationCatalogOids {
        relation: 30_000,
        row_type: Some(20_001),
        array_type: Some(30_001),
        rule: None,
    };
    assert!(TypeReferences::new(types.input(), std::iter::once(row), &memory, &cancel).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_domain_references_validate_nested_bases_and_preserve_builtin_domains() {
    let mut types = Types::with_enum();
    let domain = StoredDomain {
        object_id: [3; 16],
        oid: 30_000,
        array_oid: Some(30_001),
        identity: RelationIdentity::new("public", "selected"),
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: "public.selected".into(),
            base: mood(),
            collation: None,
            collation_oid: None,
            default: None,
            not_null: None,
            checks: Vec::new(),
        },
        array_name: Some("_selected".into()),
        usage_acl: None,
    };
    let mut column = domain.column_type();
    types
        .domains
        .insert("public.selected".into(), domain)
        .unwrap();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    index.column_type(&column).unwrap();
    let ColumnType::Domain { base, .. } = &mut column else {
        unreachable!()
    };
    **base = ColumnType::Integer;
    assert!(index.column_type(&column).is_err());
    let builtin = super::super::system_catalog_domain_with_control(13_307, &index.control())
        .unwrap()
        .unwrap();
    index.column_type(&builtin).unwrap();
    index.stored_name("domain#13307").unwrap();
    drop(builtin);
    drop(index);
    types.enums.remove("public.mood").unwrap();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    assert!(index
        .column_type(&types.domains.get("public.selected").unwrap().column_type())
        .is_err());
    drop(index);
    types
        .domains
        .edit("public.selected", |domain| {
            domain.definition.base =
                ColumnType::Array(Box::new(ColumnType::Named("missing".into())));
            Ok(())
        })
        .unwrap();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    assert!(index.column_type(&column).is_err());
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_row_type_references_preserve_relation_and_array_identity() {
    let types = Types::default();
    let row = RelationCatalogOids {
        relation: 40_000,
        row_type: Some(40_001),
        array_type: Some(40_002),
        rule: None,
    };
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::once(row), &memory, &cancel).unwrap();
    let mut reference = row
        .row_type_reference(&RelationIdentity::new("public", "rows"))
        .unwrap();
    index
        .column_type(&ColumnType::Composite(reference.clone()))
        .unwrap();
    reference.relation_oid += 3;
    assert!(index
        .column_type(&ColumnType::Composite(reference))
        .is_err());
}

#[test]
fn native_range_references_preserve_companion_identity_and_endpoint_alignment() {
    use crate::ast::RangeTypeReference;
    use crate::catalog::range_type::StoredMultirange;

    let mut types = Types::default();
    types
        .ranges
        .insert(
            "public.reading".into(),
            StoredRange {
                object_id: [3; 16],
                oid: 30_000,
                array_oid: 30_001,
                array_name: "_reading".into(),
                identity: RelationIdentity::new("public", "reading"),
                owner: RoleIdentity::BOOTSTRAP,
                subtype: ColumnType::Integer,
                subtype_operator_class: 1_978,
                collation_oid: 0,
                canonical_oid: None,
                subtype_difference_oid: None,
                multirange: StoredMultirange {
                    object_id: [4; 16],
                    oid: 30_002,
                    array_oid: 30_003,
                    array_name: "_readings".into(),
                    identity: RelationIdentity::new("public", "readings"),
                },
                usage_acl: None,
            },
        )
        .unwrap();
    let memory = MemoryBudget::new(32 << 10);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    for (oid, array_oid, multirange) in [(30_000, 30_001, false), (30_002, 30_003, true)] {
        let reference = RangeTypeReference {
            schema: "previous".into(),
            name: "previous".into(),
            oid,
            array_oid,
            alignment: RangeAlignment::for_endpoint(&ColumnType::Integer),
        };
        let make_type = |reference| {
            if multirange {
                ColumnType::DeclaredMultirange(reference)
            } else {
                ColumnType::DeclaredRange(reference)
            }
        };
        index.column_type(&make_type(reference.clone())).unwrap();
        let mut wrong = reference.clone();
        wrong.array_oid += 10;
        assert!(index.column_type(&make_type(wrong)).is_err());
        let mut wrong = reference;
        wrong.alignment = RangeAlignment::for_endpoint(&ColumnType::BigInteger);
        assert!(index.column_type(&make_type(wrong)).is_err());
    }
    assert!(index.stored_name("multirange#30000").is_err());
    assert!(index.stored_name("range#30002").is_err());
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_type_claim_admission_and_cancellation_release_original_workspace() {
    let types = Types::with_enum();
    let cancel = CancellationToken::new();
    let mut passed = 0;
    let mut denied = 0;
    for limit in (0..4096).step_by(32) {
        let memory = MemoryBudget::new(limit);
        let result = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel);
        if result.is_ok() {
            passed += 1;
        } else {
            denied += 1;
        }
        drop(result);
        assert_eq!(memory.used(), 0);
    }
    assert!(passed > 0 && denied > 0);
    let memory = MemoryBudget::new(4096);
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    cancel.cancel();
    assert!(index.column_type(&mood()).is_err());
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_concrete_type_membership_rejects_direct_and_indirect_cycles() {
    let types = Types::default();
    let rows = [
        RelationCatalogOids {
            relation: 40_000,
            row_type: Some(40_001),
            array_type: Some(40_002),
            rule: None,
        },
        RelationCatalogOids {
            relation: 50_000,
            row_type: Some(50_001),
            array_type: Some(50_002),
            rule: None,
        },
    ];
    let a = ColumnType::Composite(
        rows[0]
            .row_type_reference(&RelationIdentity::new("public", "a"))
            .unwrap(),
    );
    let b = ColumnType::Array(Box::new(ColumnType::Composite(
        rows[1]
            .row_type_reference(&RelationIdentity::new("public", "b"))
            .unwrap(),
    )));
    let memory = MemoryBudget::new(64 << 10);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), rows.into_iter(), &memory, &cancel).unwrap();
    let used = memory.used();
    index
        .validate_membership([(40_001, &b), (40_001, &b)].into_iter())
        .unwrap();
    assert_eq!(memory.used(), used);
    assert!(index
        .validate_membership([(40_001, &a)].into_iter())
        .is_err());
    assert!(index
        .validate_membership([(40_001, &b), (50_001, &a)].into_iter())
        .is_err());
    assert_eq!(memory.used(), used);
}
