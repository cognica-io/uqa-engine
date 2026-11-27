//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, RetainedValue},
    CancellationToken, EnumLabelKey, RelationIdentity,
};
use uqa_sql::catalog::{
    enum_type::{StoredEnum, StoredEnumLabel},
    roles::RoleIdentity,
};

fn catalog() -> CatalogReadView {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    for (oid, count) in [(40000, 2), (50000, 16384)] {
        let definition = StoredEnum {
            object_id: [u8::try_from(oid / 10000).unwrap(); 16],
            oid,
            array_oid: oid + 1,
            array_name: format!("_enum{oid}"),
            identity: RelationIdentity::new("public", format!("enum{oid}")),
            owner: RoleIdentity::BOOTSTRAP,
            usage_acl: None,
            labels: EnumLabelKey::initial(count)
                .unwrap()
                .into_iter()
                .enumerate()
                .map(|(position, key)| StoredEnumLabel {
                    oid: oid + 2 + 2 * position as u32,
                    key,
                    label: format!("label_{position:050}"),
                    sort_order: position as f32,
                })
                .collect(),
        };
        RetainedValue::make_mut(&mut snapshot.definitions.enums)
            .insert(format!("public.enum{oid}"), definition)
            .unwrap();
    }
    CatalogReadView::new(snapshot)
}

#[test]
fn native_partition_value_metadata_admits_only_selected_labels_and_retires_scratch() {
    let catalog = catalog();
    let mut outcomes = (0, 0);
    for limit in [0, 128, 4096] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        {
            let values = PartitionValues::new(
                &catalog,
                None,
                ProductionControl::new(&memory, &token, &token),
            );
            match values.enum_type_labels(40000) {
                Ok(Some(labels)) => {
                    outcomes.0 += 1;
                    assert_eq!(labels.labels.len(), 2);
                    let used = memory.used();
                    assert!(Arc::ptr_eq(
                        &labels,
                        &values.enum_type_labels(40000).unwrap().unwrap()
                    ));
                    assert_eq!(memory.used(), used);
                    assert_eq!(values.enum_label_position(40004).unwrap(), Some((40000, 1)));
                    token.cancel();
                    assert!(values.enum_type_labels(40000).is_err());
                }
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"));
                    outcomes.1 += 1;
                }
                Ok(None) => panic!("original enum exists"),
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(outcomes.0 > 0 && outcomes.1 > 0);
}

#[test]
fn native_partition_range_comparison_uses_original_support_without_canonicalization() {
    use uqa_core::Value;
    use uqa_sql::{
        catalog::range_type::{StoredMultirange, StoredRange},
        ColumnType,
    };
    let mut snapshot = catalog().snapshot().clone();
    let definition = StoredRange {
        object_id: [7; 16],
        oid: 91000,
        array_oid: 91001,
        array_name: "_original_range".into(),
        identity: RelationIdentity::new("public", "original_range"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: Some(99999),
        subtype_difference_oid: None,
        constructor_oids: None,
        multirange: StoredMultirange {
            object_id: [8; 16],
            oid: 91002,
            array_oid: 91003,
            array_name: "_original_multirange".into(),
            identity: RelationIdentity::new("public", "original_multirange"),
        },
        usage_acl: None,
    };
    RetainedValue::make_mut(&mut snapshot.definitions.ranges)
        .insert("public.original_range".into(), definition)
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    {
        let values = PartitionValues::new(&catalog, None, control);
        for oid in [91000, 91002] {
            let descriptor = values.value_range_type(oid).unwrap().unwrap();
            assert_eq!(descriptor.endpoint, ColumnType::Integer);
            assert_eq!(
                values
                    .compare_range_bounds(oid, &Value::Int(1), &Value::Int(2), &control)
                    .unwrap(),
                std::cmp::Ordering::Less
            );
            assert_eq!(
                values
                    .compare_range_bounds(oid, &Value::Int(2), &Value::Int(2), &control)
                    .unwrap(),
                std::cmp::Ordering::Equal
            );
        }
        token.cancel();
        assert!(values
            .compare_range_bounds(91000, &Value::Int(1), &Value::Int(2), &control)
            .is_err());
    }
    assert_eq!(memory.used(), 0);
}
