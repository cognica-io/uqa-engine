//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::CreateDomain;
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

fn domain() -> StoredDomain {
    StoredDomain {
        object_id: [1; 16],
        oid: 16_500,
        array_oid: Some(16_501),
        identity: RelationIdentity::new("domain_schema", "domain_name"),
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: "domain_schema.domain_name".into(),
            base: ColumnType::Array(Box::new(ColumnType::Named("nested_payload".repeat(32)))),
            collation: None,
            collation_oid: Some(100),
            default: None,
            not_null: None,
            checks: Vec::new(),
        },
        array_name: None,
        usage_acl: None,
    }
}

#[test]
fn catalog_type_projection_retains_exact_names_and_nested_domain_storage() {
    let domain = domain();
    let enumeration = StoredEnum {
        object_id: [2; 16],
        oid: 16_502,
        array_oid: 16_503,
        array_name: "_mood".into(),
        identity: RelationIdentity::new("enum_schema", "mood"),
        owner: (),
        labels: Vec::new(),
        usage_acl: None,
    };
    let composite = StoredComposite {
        object_id: [3; 16],
        oid: 16_504,
        relation_oid: 16_505,
        array_oid: 16_506,
        array_name: "_record".into(),
        identity: RelationIdentity::new("record_schema", "record"),
        owner: (),
        attributes: Vec::new(),
        usage_acl: None,
    };
    let range = StoredRange {
        object_id: [4; 16],
        oid: 16_507,
        array_oid: 16_508,
        array_name: "_range".into(),
        identity: RelationIdentity::new("range_schema", "range"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1_978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: super::super::range_type::StoredMultirange {
            object_id: [5; 16],
            oid: 16_509,
            array_oid: 16_510,
            array_name: "_ranges".into(),
            identity: RelationIdentity::new("multi_schema", "ranges"),
        },
        usage_acl: None,
    };
    let expected = [
        domain.column_type(),
        enumeration.column_type(),
        composite.column_type(),
        range.column_type(),
        range.multirange_type(),
    ];
    for (index, expected) in expected.iter().enumerate() {
        let budget = MemoryBudget::new(8192);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut output = None;
        let allocated = allocation_counter::measure(|| {
            output = Some(
                match index {
                    0 => domain.column_type_with_control(&control),
                    1 => enumeration.column_type_with_control(&control),
                    2 => composite.column_type_with_control(&control),
                    3 => range.column_type_with_control(&control),
                    _ => range.multirange_type_with_control(&control),
                }
                .unwrap(),
            );
        });
        assert_eq!(&**output.as_ref().unwrap(), expected);
        assert_eq!(budget.used() as i64, allocated.bytes_current);
        assert_eq!(budget.used(), output.as_ref().unwrap().reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn catalog_type_projection_rejects_each_partial_destination_and_releases_its_owner() {
    let domain = domain();
    let cancellation = CancellationToken::new();
    let baseline = MemoryBudget::new(8192);
    let control = ProductionControl::new(&baseline, &cancellation, &cancellation);
    let required = domain
        .column_type_with_control(&control)
        .unwrap()
        .reserved_bytes();
    assert_eq!(baseline.used(), 0);
    for available in 0..=required {
        let budget = MemoryBudget::new(available);
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let output = domain.column_type_with_control(&control);
        if available < required {
            assert!(output.is_err(), "allowance {available}");
        } else {
            assert_eq!(&*output.unwrap(), &domain.column_type());
        }
        assert_eq!(budget.used(), 0, "allowance {available}");
    }
}

#[test]
fn relation_type_projection_checks_both_cancellation_scopes_before_copying_names() {
    let identity = RelationIdentity::new("schema", "relation");
    let oids = RelationCatalogOids {
        relation: 20_000,
        row_type: Some(20_001),
        array_type: Some(20_002),
        rule: None,
    };
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        // Initialize the token's platform mutex before measuring catalog output allocations.
        assert!(control.check().is_err());
        let allocated = allocation_counter::measure(|| {
            assert!(oids
                .row_type_reference_with_control(&identity, &control)
                .is_err());
        });
        assert_eq!(allocated.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let output = oids
        .row_type_reference_with_control(&identity, &control)
        .unwrap()
        .unwrap();
    assert_eq!(&*output, &oids.row_type_reference(&identity).unwrap());
    assert_eq!(budget.used(), identity.schema.len() + identity.name.len());
    drop(output);
    assert_eq!(budget.used(), 0);
}
