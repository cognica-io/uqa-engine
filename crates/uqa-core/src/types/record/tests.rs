//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken,
};

#[test]
fn named_record_identity_survives_json_and_controlled_copies() {
    let fields = vec![("n".into(), Value::Int(7))];
    let legacy = Value::Record(RecordValue::new(fields.clone()));
    let typed = Value::Record(RecordValue::from_parts(fields, Some(20_000)));
    assert_eq!(legacy, typed);
    assert!(!legacy.has_same_representation(&typed));
    for value in [legacy, typed] {
        let json = serde_json::to_string(&value).unwrap();
        let restored: Value = serde_json::from_str(&json).unwrap();
        assert!(restored.has_same_representation(&value));
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let copied = control.copy_value(&value).unwrap();
        assert!(copied.has_same_representation(&value));
        assert_eq!(memory.used(), copied.reserved_bytes());
        assert!(memory.used() >= RecordValue::retained_header_bytes());
        drop(copied);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn record_header_is_admitted_before_allocation_and_released_on_failure() {
    let token = CancellationToken::new();
    for limit in [0, RecordValue::retained_header_bytes() - 1, 4096] {
        let memory = MemoryBudget::new(limit);
        let control = ProductionControl::new(&memory, &token, &token);
        let fields = ProductionVec::new(control).finish().unwrap();
        let result = RecordValue::with_control(fields, Some(20_000), &control);
        assert_eq!(result.is_ok(), limit == 4096);
        if let Ok(value) = result {
            assert_eq!(value.type_oid(), Some(20_000));
            assert_eq!(
                memory.used(),
                RecordValue::retained_header_bytes()
                    + value.descriptor().retained_buffer_bytes().unwrap()
            );
        }
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &token, &token);
    let fields = ProductionVec::new(control).finish().unwrap();
    token.cancel();
    assert!(RecordValue::with_control(fields, None, &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn named_record_clones_share_names_and_keep_mutable_values_independent() {
    let names = (0..128)
        .map(|index| format!("{index}{}", "x".repeat(4096)))
        .collect();
    let descriptor = RecordDescriptor::new(names, Some(24_000));
    let source =
        RecordValue::from_shared_parts(vec![Value::Null; 128], descriptor.clone()).unwrap();
    let mut copied = None;
    let allocated = allocation_counter::measure(|| {
        copied = Some(source.clone());
    });
    let mut copied = copied.unwrap();
    assert!(
        allocated.bytes_total <= (128 * size_of::<Value>() + 256) as u64,
        "{allocated:?}"
    );
    assert!(copied.descriptor().shares_storage_with(&descriptor));
    assert_ne!(source.values().as_ptr(), copied.values().as_ptr());
    copied.values_mut()[0] = Value::Int(9);
    assert_eq!(source.values()[0], Value::Null);
    drop((source, descriptor));
    assert_eq!(copied.type_oid(), Some(24_000));
    assert_eq!(copied.descriptor().field_names()[127].len(), 4099);
}

#[test]
fn controlled_record_copies_share_names_and_charge_all_logical_metadata() {
    let descriptor = RecordDescriptor::new(vec!["n".repeat(4096); 8], Some(24_000));
    let source = Value::Record(
        RecordValue::from_shared_parts(vec![Value::Null; 8], descriptor.clone()).unwrap(),
    );
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let copy = source.clone_budgeted(&budget, &token).unwrap();
    let Value::Record(record) = &*copy else {
        panic!("record")
    };
    assert!(record.descriptor().shares_storage_with(&descriptor));
    let expected = record.retained_buffer_bytes().unwrap() + 8 * 4096;
    assert_eq!(copy.reserved_bytes(), expected);
    assert_eq!(
        source.retained_payload_bytes(&budget, &token).unwrap(),
        expected
    );
    let small = MemoryBudget::new(expected - 1);
    assert!(matches!(
        source.clone_budgeted(&small, &token),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(small.used(), 0);
    drop(copy);
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert!(matches!(
        source.clone_budgeted(&budget, &token),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn shared_record_names_preserve_duplicate_order_identity_and_owned_conversion() {
    let record = RecordValue::from_parts(
        vec![("same".into(), Value::Int(1)), ("same".into(), Value::Null)],
        Some(24_000),
    );
    let copied = record.clone();
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(
        json,
        r#"{"$uqa_type":"record","fields":[["same",1],["same",null]],"type_oid":24000}"#
    );
    let (mut fields, oid) = record.into_parts();
    fields[0].0 = "changed".into();
    assert_eq!(copied.descriptor().field_names(), &["same", "same"]);
    assert_eq!(oid, Some(24_000));
    let (values, descriptor) = copied.clone().into_shared_parts();
    let reconstructed = RecordValue::from_shared_parts(values, descriptor).unwrap();
    assert!(reconstructed
        .descriptor()
        .shares_storage_with(copied.descriptor()));
    let changed_oid = copied.clone().with_type_oid(Some(24_001));
    assert_eq!(copied, changed_oid);
    assert!(!Value::Record(copied.clone()).has_same_representation(&Value::Record(changed_oid)));
    assert!(RecordValue::from_shared_parts(vec![], copied.descriptor().clone()).is_err());
    let empty = RecordValue::from_parts(vec![], Some(24_000));
    assert_eq!(empty.type_oid(), Some(24_000));
    assert_ne!(
        serde_json::to_string(&empty).unwrap(),
        serde_json::to_string(&RecordValue::default()).unwrap()
    );
}

#[test]
fn controlled_record_construction_moves_payloads_and_releases_temporary_pairs() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut fields = ProductionVec::new(control);
    fields.reserve(16).unwrap();
    let (name, name_memory) = control.copy_text(&"n".repeat(2048)).unwrap().into_parts();
    let pointer = name.as_ptr();
    let (value, memory) = control
        .copy_value(&Value::Str("value".into()))
        .unwrap()
        .into_parts();
    fields
        .push_produced(
            control
                .finish((name, value), control.combine(name_memory, memory))
                .unwrap(),
        )
        .unwrap();
    let produced =
        RecordValue::with_control(fields.finish().unwrap(), Some(24_000), &control).unwrap();
    assert_eq!(produced.descriptor().field_names()[0].as_ptr(), pointer);
    assert_eq!(
        produced.reserved_bytes(),
        produced.retained_buffer_bytes().unwrap() + 2048 + 5
    );
    let (record, memory) = produced.into_parts();
    let (values, descriptor) = record.into_shared_parts();
    drop((values, memory));
    assert_eq!(budget.used(), 0);
    let mut output = ProductionVec::new(control);
    output
        .push_produced(
            control
                .finish(Value::Null, control.empty_reservation())
                .unwrap(),
        )
        .unwrap();
    let output = output.finish().unwrap();
    let pointer = output.as_ptr();
    let value = RecordValue::with_shared_control(output, descriptor.clone(), &control).unwrap();
    assert_eq!(value.values().as_ptr(), pointer);
    assert!(value.descriptor().shares_storage_with(&descriptor));
    drop(value);
    assert_eq!(budget.used(), 0);
}

#[test]
fn exclusive_record_owned_conversion_moves_names_without_copying_payloads() {
    let name = "name".repeat(8192);
    let pointer = name.as_ptr();
    let record = RecordValue::new(vec![(name, Value::Null)]);
    let mut fields = None;
    let allocated = allocation_counter::measure(|| {
        fields = Some(record.into_parts().0);
    });
    let fields = fields.unwrap();
    assert_eq!(fields[0].0.as_ptr(), pointer);
    assert!(allocated.bytes_total < 1024, "{allocated:?}");
}
