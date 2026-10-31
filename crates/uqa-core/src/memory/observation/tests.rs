//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::CancellationToken;

#[test]
fn observation_reads_live_lengths_and_capacities_without_counting_its_workspace() {
    let mut text = String::with_capacity(128);
    text.push_str("abc");
    let mut values = Vec::with_capacity(12);
    values.push(text);
    let expected = AllocationUsage {
        used_bytes: size_of::<String>() + 3,
        allocated_bytes: values.capacity() * size_of::<String>() + values[0].capacity(),
    };
    let budget = MemoryBudget::new(4096);
    let sibling = budget.reserve(1000).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        AllocationUsage::measure(&values, &control).unwrap(),
        expected
    );
    assert_eq!(budget.used(), sibling.bytes());
    values.clear();
    let empty = AllocationUsage::measure(&values, &control).unwrap();
    assert_eq!(empty.used_bytes, 0);
    assert_eq!(
        empty.allocated_bytes,
        values.capacity() * size_of::<String>()
    );
}

#[test]
fn observation_rejects_workspace_and_cancellation_without_retaining_a_lease() {
    let values = vec![String::from("retained source")];
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    assert!(matches!(
        AllocationUsage::measure(
            &values,
            &ProductionControl::new(&budget, &original, &invoking)
        ),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
    invoking.cancel();
    assert!(matches!(
        AllocationUsage::measure(
            &values,
            &ProductionControl::new(&budget, &original, &invoking)
        ),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(values[0], "retained source");
}

#[test]
fn observation_counts_shared_tuple_storage_once_without_reading_it() {
    let mut bytes = Vec::with_capacity(1 << 20);
    bytes.extend_from_slice(b"uninterpreted tuple");
    let (used, allocated) = (bytes.len(), bytes.capacity());
    let datum = crate::DatumValue::new(999_999, 0, bytes);
    let values = vec![crate::Value::Datum(datum); 128];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let actual = AllocationUsage::measure(&values, &control).unwrap();
    assert_eq!(
        actual.used_bytes,
        values.len() * size_of::<crate::Value>() + size_of::<Vec<u8>>() + used
    );
    assert_eq!(
        actual.allocated_bytes,
        values.capacity() * size_of::<crate::Value>() + size_of::<Vec<u8>>() + allocated
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn observation_workspace_follows_depth_instead_of_sequence_width() {
    let wide = vec![String::new(); 100_000];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        AllocationUsage::measure(&wide, &control)
            .unwrap()
            .used_bytes,
        wide.len() * size_of::<String>()
    );
    assert!(budget.peak() < 4096);
    assert_eq!(budget.used(), 0);

    let fields: std::collections::BTreeMap<_, _> = (0..10_000)
        .map(|index| (index.to_string(), crate::Value::Int(index)))
        .collect();
    let key_bytes: usize = fields.keys().map(String::len).sum();
    let document = crate::Value::Map(fields);
    assert_eq!(
        AllocationUsage::measure(&document, &control)
            .unwrap()
            .used_bytes,
        key_bytes
    );
    assert!(budget.peak() < 4096);
    assert_eq!(budget.used(), 0);

    let mut deep = crate::Value::Null;
    for _ in 0..4000 {
        deep = crate::Value::List(vec![deep]);
    }
    let measured = AllocationUsage::measure(&deep, &ProductionControl::uncontrolled()).unwrap();
    assert_eq!(measured.used_bytes, 4000 * size_of::<crate::Value>());
    while let crate::Value::List(mut values) = deep {
        deep = values.pop().unwrap();
    }
}
