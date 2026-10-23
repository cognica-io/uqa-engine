//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken, JsonValueDecoder,
};

fn field(oid: u32) -> RecordFieldType {
    RecordFieldType {
        collation_oid: None,
        oid,
        type_modifier: -1,
    }
}

#[test]
fn row_collations_survive_json_copy_and_both_cancellation_owners() {
    let fields: Vec<_> = [None, Some(0), Some(950), Some(963), Some(u32::MAX)]
        .into_iter()
        .map(|collation_oid| RecordFieldType {
            oid: 25,
            type_modifier: -1,
            collation_oid,
        })
        .collect();
    let source =
        Value::Row(RowValue::typed(vec![Value::Null; fields.len()], fields.clone()).unwrap());
    let encoded = serde_json::to_string(&source).unwrap();
    let ordinary: Value = serde_json::from_str(&encoded).unwrap();
    assert!(source.has_same_representation(&ordinary));
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let decoded = JsonValueDecoder::new(&budget, &original)
        .value(&encoded)
        .unwrap();
    assert!(source.has_same_representation(&decoded));
    drop(decoded);
    let copied = control.copy_value(&source).unwrap();
    assert!(source.has_same_representation(&copied));
    assert_eq!(budget.used(), copied.reserved_bytes());
    drop(copied);
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut values = ProductionVec::new(control);
        let mut types = ProductionVec::new(control);
        for field in &fields {
            values
                .push_produced(control.copy_value(&Value::Null).unwrap())
                .unwrap();
            types.push_copy(*field).unwrap();
        }
        let values = values.finish().unwrap();
        let types = types.finish().unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            RowValue::typed_with_control(values, types, &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
    assert!(JsonValueDecoder::new(&MemoryBudget::new(0), &original)
        .value(&encoded)
        .is_err());
}

fn typed(oid: u32) -> Value {
    Value::Row(
        RowValue::typed(
            vec![Value::Int(1), Value::Null],
            vec![field(oid), field(25)],
        )
        .unwrap(),
    )
}

#[test]
fn descriptors_distinguish_representation_without_changing_value_order() {
    let integer = typed(23);
    let bigint = typed(20);
    let unknown = Value::Row(vec![Value::Int(1), Value::Null].into());
    assert_eq!(integer, bigint);
    assert_eq!(integer, unknown);
    assert_eq!(integer.cmp(&bigint), std::cmp::Ordering::Equal);
    assert!(!integer.has_same_representation(&bigint));
    assert!(!integer.has_same_representation(&unknown));
    assert!(integer.has_same_representation(&integer.clone()));
    let with_modifier = Value::Row(
        RowValue::typed(
            vec![Value::Int(1), Value::Null],
            vec![
                field(23),
                RecordFieldType {
                    collation_oid: None,
                    oid: 25,
                    type_modifier: 8,
                },
            ],
        )
        .unwrap(),
    );
    assert_eq!(integer, with_modifier);
    assert!(!integer.has_same_representation(&with_modifier));
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    assert_eq!(
        integer.cmp_with_control(&bigint, &control).unwrap(),
        std::cmp::Ordering::Equal
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn row_width_is_validated_and_value_transformations_keep_the_descriptor() {
    assert!(matches!(
        RowValue::typed(vec![Value::Null], Vec::new()),
        Err(ValueRetentionError::Malformed { kind: "row", .. })
    ));
    let row = RowValue::typed(vec![Value::Int(1)], vec![field(23)]).unwrap();
    let mut changed = row.clone().with_values(vec![Value::Int(2)]).unwrap();
    changed[0] = Value::Int(3);
    assert_eq!(changed.field_types(), Some([field(23)].as_slice()));
    let (values, types) = changed.into_parts();
    assert_eq!(values, vec![Value::Int(3)]);
    assert_eq!(types, Some(vec![field(23)]));
    assert!(row.with_values(Vec::new()).is_err());
    let plain: RowValue = [Value::Int(1), Value::Null].into_iter().collect();
    assert_eq!(plain.into_values(), vec![Value::Int(1), Value::Null]);
}

#[test]
fn row_tags_preserve_legacy_values_and_round_trip_type_metadata() {
    let old = r#"{"$uqa_type":"row","values":[1,null]}"#;
    let legacy: Value = serde_json::from_str(old).unwrap();
    let Value::Row(row) = &legacy else {
        panic!("row tag");
    };
    assert_eq!(row.field_types(), None);
    assert_eq!(serde_json::to_string(&legacy).unwrap(), old);
    for original in [
        typed(23),
        typed(20),
        Value::Row(RowValue::typed(Vec::new(), Vec::new()).unwrap()),
    ] {
        let text = serde_json::to_string(&original).unwrap();
        let decoded: Value = serde_json::from_str(&text).unwrap();
        assert!(original.has_same_representation(&decoded));
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let decoded = JsonValueDecoder::new(&budget, &cancellation)
            .value(&text)
            .unwrap();
        assert!(original.has_same_representation(&decoded));
        assert_eq!(
            decoded.reserved_bytes(),
            decoded
                .retained_payload_bytes(&budget, &cancellation)
                .unwrap()
        );
        assert_eq!(budget.used(), decoded.reserved_bytes());
        drop(decoded);
        assert_eq!(budget.used(), 0);
    }
    let row = RowValue::typed(vec![Value::Null], vec![field(705)]).unwrap();
    let decoded: RowValue = serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
    assert_eq!(decoded.field_types(), row.field_types());
}

#[test]
fn malformed_row_descriptors_fail_both_decoders_and_release_their_allowance() {
    for text in [
        r#"{"$uqa_type":"row","values":[1],"field_types":[]}"#,
        r#"{"$uqa_type":"row","values":[],"field_types":[{"oid":23,"type_modifier":-1}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":-1,"type_modifier":-1}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":23,"type_modifier":2147483648}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":23}]}"#,
        r#"{"$uqa_type":"row","values":null,"field_types":[]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":25,"type_modifier":-1,"collation_oid":-1}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":25,"type_modifier":-1,"collation_oid":4294967296}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":25,"type_modifier":-1,"collation_oid":"C"}]}"#,
        r#"{"$uqa_type":"row","values":[1],"field_types":[{"oid":25,"type_modifier":-1,"collation":950}]}"#,
    ] {
        assert!(serde_json::from_str::<Value>(text).is_err(), "{text}");
        let budget = MemoryBudget::new(1 << 20);
        let previous = budget.reserve(17).unwrap();
        assert!(
            JsonValueDecoder::new(&budget, &CancellationToken::new())
                .value(text)
                .is_err(),
            "{text}"
        );
        assert_eq!(budget.used(), previous.bytes());
    }
    assert!(serde_json::from_str::<RowValue>(
        r#"{"$uqa_type":"row","values":[1],"field_types":[]}"#
    )
    .is_err());
}

#[test]
fn row_copies_charge_descriptors_and_headers_without_adopting_spare_capacity() {
    let mut values = Vec::with_capacity(32);
    values.push(Value::Null);
    let mut types = Vec::with_capacity(64);
    types.push(field(23));
    let retained = RowValue::decoded_header_bytes()
        + values.capacity() * size_of::<Value>()
        + types.len() * size_of::<RecordFieldType>();
    let source = Value::Row(RowValue::typed(values, types).unwrap());
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let lease = source
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert_eq!(lease.bytes(), retained);
    let copied = source.clone_budgeted(&budget, &cancellation).unwrap();
    assert!(source.has_same_representation(&copied));
    assert_eq!(
        copied.reserved_bytes(),
        RowValue::decoded_header_bytes() + size_of::<Value>() + size_of::<RecordFieldType>()
    );
    assert_eq!(budget.used(), retained + copied.reserved_bytes());
    drop((lease, copied));
    assert_eq!(budget.used(), 0);
    assert!(matches!(
        Value::Row(Vec::new().into()).clone_budgeted(&MemoryBudget::new(0), &cancellation),
        Err(ValueRetentionError::Memory(_))
    ));
}

#[test]
fn controlled_row_construction_transfers_buffers_and_releases_rejected_inputs() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let values = control
        .copy_value(&Value::List(vec![Value::Str("kept".into())]))
        .unwrap();
    let (Value::List(values), memory) = values.into_parts() else {
        panic!("list values");
    };
    let pointer = values.as_ptr();
    let values = control.finish(values, memory).unwrap();
    let mut types = ProductionVec::new(control);
    types.push_copy(field(25)).unwrap();
    let types = types.finish().unwrap();
    let row = RowValue::typed_with_control(values, types, &control).unwrap();
    assert_eq!(row.values().as_ptr(), pointer);
    assert_eq!(row.field_types().unwrap(), &[field(25)]);
    assert_eq!(
        row.reserved_bytes(),
        row.retained_buffer_bytes().unwrap() + "kept".len()
    );
    drop(row);
    assert_eq!(budget.used(), 0);

    for cancel in [false, true] {
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut values = ProductionVec::new(control);
        values
            .push_produced(control.copy_value(&Value::Null).unwrap())
            .unwrap();
        let values = values.finish().unwrap();
        let types = ProductionVec::new(control).finish().unwrap();
        if cancel {
            cancellation.cancel();
        }
        assert!(RowValue::typed_with_control(values, types, &control).is_err());
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(RowValue::decoded_header_bytes() - 1);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let values = ProductionVec::new(control).finish().unwrap();
    assert!(matches!(
        RowValue::new_with_control(values, &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn anonymous_row_production_preserves_ownership_and_releases_cancelled_inputs() {
    for values in [Vec::new(), vec![Value::Str("kept".into()), Value::Null]] {
        let budget = MemoryBudget::new(4096);
        let previous = budget.reserve(17).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let admitted = control.copy_value(&Value::List(values.clone())).unwrap();
        let (Value::List(admitted), memory) = admitted.into_parts() else {
            panic!("list values");
        };
        let pointer = admitted.as_ptr();
        let admitted = control.finish(admitted, memory).unwrap();
        let row = RowValue::new_with_control(admitted, &control).unwrap();
        assert_eq!(row.values(), values);
        assert_eq!(row.values().as_ptr(), pointer);
        assert_eq!(row.field_types(), None);
        assert_eq!(budget.used(), previous.bytes() + row.reserved_bytes());
        assert_eq!(
            row.reserved_bytes(),
            row.retained_buffer_bytes().unwrap() + if values.is_empty() { 0 } else { "kept".len() }
        );
        drop(row);
        assert_eq!(budget.used(), previous.bytes());
    }

    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut values = ProductionVec::new(control);
        values
            .push_produced(control.copy_value(&Value::Str("released".into())).unwrap())
            .unwrap();
        let values = values.finish().unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            RowValue::new_with_control(values, &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn row_copy_and_decode_cancellation_release_descriptor_storage() {
    let source =
        Value::Row(RowValue::typed(vec![Value::Null; 1024], vec![field(23); 1024]).unwrap());
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut total = 0;
    drop(
        source
            .clone_budgeted_with_check(&budget, || {
                total += 1;
                cancellation.check()
            })
            .unwrap(),
    );
    for fail_at in [1, total - 1, total] {
        let cancellation = CancellationToken::new();
        let mut checks = 0;
        assert!(matches!(
            source.clone_budgeted_with_check(&budget, || {
                checks += 1;
                if checks == fail_at {
                    cancellation.cancel();
                }
                cancellation.check()
            }),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(checks, fail_at);
        assert_eq!(budget.used(), 0);
    }
    cancellation.cancel();
    assert!(JsonValueDecoder::new(&budget, &cancellation)
        .value(&serde_json::to_string(&source).unwrap())
        .is_err());
    assert_eq!(budget.used(), 0);
}
