//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, RowValue, Value};

#[test]
fn controlled_shared_row_construction_moves_values_and_retains_descriptor_identity() {
    let descriptor = descriptor(1);
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let values = control
        .copy_value(&Value::List(vec![Value::Str("kept".into())]))
        .unwrap();
    let (Value::List(values), memory) = values.into_parts() else {
        panic!("list")
    };
    let pointer = values.as_ptr();
    let values = control.finish(values, memory).unwrap();
    let output = RowValue::typed_shared_with_control(values, descriptor.clone(), &control).unwrap();
    assert_eq!(output.values().as_ptr(), pointer);
    assert!(output
        .descriptor()
        .unwrap()
        .shares_storage_with(&descriptor));
    assert_eq!(
        output.reserved_bytes(),
        output.retained_buffer_bytes().unwrap() + 4
    );
    drop(output);
    assert_eq!(budget.used(), 0);
    assert_eq!(descriptor.field_types()[0].oid, 25);
}

fn descriptor(width: usize) -> RowDescriptor {
    RowDescriptor::new(
        (0..width)
            .map(|index| RecordFieldType {
                oid: 25,
                type_modifier: index as i32,
                collation_oid: Some(950),
            })
            .collect(),
    )
}

#[test]
fn anonymous_row_clones_share_descriptor_allocations_and_keep_values_independent() {
    let descriptor = descriptor(4096);
    let source = RowValue::typed_shared(vec![Value::Null; 4096], descriptor.clone()).unwrap();
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(source.clone());
    });
    let mut output = output.unwrap();
    assert!(
        allocated.bytes_total <= (4096 * size_of::<Value>() + 256) as u64,
        "{allocated:?}"
    );
    assert_ne!(source.values().as_ptr(), output.values().as_ptr());
    assert!(output
        .descriptor()
        .unwrap()
        .shares_storage_with(&descriptor));
    output[0] = Value::Int(7);
    assert_eq!(source[0], Value::Null);
    assert_eq!(output.field_types(), source.field_types());
    drop((source, descriptor));
    assert_eq!(output.field_types().unwrap()[4095].type_modifier, 4095);
}

#[test]
fn controlled_anonymous_row_copies_share_metadata_without_weakening_quotas() {
    let descriptor = descriptor(64);
    let source =
        Value::Row(RowValue::typed_shared(vec![Value::Null; 64], descriptor.clone()).unwrap());
    let cancellation = CancellationToken::new();
    let budget = MemoryBudget::new(64 << 10);
    let copy = source.clone_budgeted(&budget, &cancellation).unwrap();
    let Value::Row(row) = &*copy else {
        panic!("row copy")
    };
    assert!(row.descriptor().unwrap().shares_storage_with(&descriptor));
    assert!(source.has_same_representation(&copy));
    assert_eq!(copy.reserved_bytes(), row.retained_buffer_bytes().unwrap());
    let too_small = MemoryBudget::new(row.retained_buffer_bytes().unwrap() - 1);
    assert!(matches!(
        source.clone_budgeted(&too_small, &cancellation),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(too_small.used(), 0);
    drop(copy);
    assert_eq!(budget.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        source.clone_budgeted(&budget, &cancellation),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn shared_anonymous_descriptors_preserve_owned_conversion_and_zero_field_identity() {
    let descriptor = descriptor(2);
    let first =
        RowValue::typed_shared(vec![Value::Int(1), Value::Null], descriptor.clone()).unwrap();
    let second = first.clone();
    let expected = serde_json::to_string(&first).unwrap();
    let (values, types) = first.into_parts();
    let mut types = types.unwrap();
    types[0].oid = 23;
    assert_eq!(second.field_types().unwrap()[0].oid, 25);
    assert_eq!(serde_json::to_string(&second).unwrap(), expected);
    assert!(RowValue::typed_shared(Vec::new(), descriptor).is_err());
    assert_eq!(values, [Value::Int(1), Value::Null]);
    let empty = RowValue::typed_shared(Vec::new(), RowDescriptor::new(Vec::new())).unwrap();
    assert_eq!(empty.field_types(), Some(&[][..]));
    assert_eq!(RowValue::new(Vec::new()).field_types(), None);
    assert_ne!(
        serde_json::to_string(&empty).unwrap(),
        serde_json::to_string(&RowValue::new(Vec::new())).unwrap()
    );
    let (values, descriptor) = second.into_shared_parts();
    let rebuilt = RowValue::typed_shared(values, descriptor.unwrap()).unwrap();
    assert_eq!(serde_json::to_string(&rebuilt).unwrap(), expected);
}
