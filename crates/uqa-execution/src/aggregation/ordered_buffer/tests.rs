//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::{
    finalize::finish_aggregate, AggregateAccumulator, AggregateValueBuffer, AggregateValueRecord,
    RegisteredAggregateBuffer, RegisteredAggregateRecord,
};
use std::cell::Cell;
use std::io::Cursor;

#[test]
fn resident_scalar_and_registered_arguments_are_sized_without_allocating() {
    let mut scalar = AggregateValueBuffer::new(usize::MAX);
    let mut registered = RegisteredAggregateBuffer::new(usize::MAX);
    let rows = 10_000;
    scalar.rows.reserve_exact(rows);
    registered.rows.reserve_exact(rows);
    let inputs = (0..rows)
        .map(|index| {
            let value = Value::List(vec![Value::Str(format!("value {index}")), Value::Null]);
            let keys = vec![AggregateSortKey::ascending(Value::Int(index as i64))];
            (
                value.clone(),
                keys.clone(),
                vec![value, Value::Bool(false)],
                keys,
            )
        })
        .collect::<Vec<_>>();
    let mut inputs = inputs.into_iter();
    let allocations = allocation_counter::measure(|| {
        for (value, keys, arguments, registered_keys) in &mut inputs {
            scalar.push(value, keys, None).unwrap();
            registered.push(arguments, registered_keys, None).unwrap();
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(scalar.rows.len(), rows);
    assert_eq!(registered.rows.len(), rows);
    assert!(registered.memory_bytes > scalar.memory_bytes);
    assert!(scalar.runs.is_empty() && registered.runs.is_empty());
}

#[test]
fn completed_groups_move_resident_values_and_window_reads_remain_repeatable() {
    let mut accumulator = AggregateAccumulator::builtin("array_agg");
    let text = "original allocation".repeat(1024);
    let pointer = text.as_ptr();
    accumulator
        .buffers_mut()
        .values
        .push(Value::Str(text), Vec::new(), None)
        .unwrap();
    for _ in 0..2 {
        let value = crate::aggregation::aggregate_value("array_agg", &accumulator, None).unwrap();
        let Value::Array(array) = value else {
            panic!("expected an array")
        };
        let [Value::Str(text)] = array.elements() else {
            panic!("expected one text element")
        };
        assert_ne!(text.as_ptr(), pointer);
    }
    let value = finish_aggregate("array_agg", &mut accumulator, &[], None).unwrap();
    let Value::Array(array) = value else {
        panic!("expected an array")
    };
    let [Value::Str(text)] = array.elements() else {
        panic!("expected one text element")
    };
    assert_eq!(text.as_ptr(), pointer);
}

#[test]
fn typed_spill_keeps_float_bits_null_order_and_registered_argument_boundaries() {
    let nan_bits = 0x7ff8_0000_0000_0abc;
    let record = RegisteredAggregateRecord::new(
        vec![
            Value::Null,
            Value::Float(f64::from_bits(nan_bits)),
            Value::List(vec![Value::Bytes(vec![0, 255])]),
        ],
        vec![AggregateSortKey {
            value: Value::Null,
            descending: false,
            nulls_first: true,
        }],
        u64::MAX - 1,
    );
    let mut bytes = Vec::new();
    let size = codec::write(&mut bytes, &record).unwrap();
    assert_eq!(size, bytes.len());
    let mut stream = Cursor::new(&bytes);
    let restored = codec::read::<RegisteredAggregateRecord>(&mut stream, size)
        .unwrap()
        .unwrap();
    assert!(codec::read::<RegisteredAggregateRecord>(&mut stream, size)
        .unwrap()
        .is_none());
    assert_eq!(restored.sequence(), record.sequence());
    assert!(restored.keys()[0].nulls_first && !restored.keys()[0].descending);
    assert!(matches!(restored.keys()[0].value, Value::Null));
    let [Value::Null, Value::Float(nan), nested] = restored.inputs() else {
        panic!("incorrect argument layout")
    };
    assert_eq!(nan.to_bits(), nan_bits);
    assert_eq!(nested, &record.values[2]);

    let error = |bytes: &[u8], bound| {
        codec::read::<RegisteredAggregateRecord>(&mut Cursor::new(bytes), bound)
            .err()
            .unwrap()
            .to_string()
    };
    assert!(error(&bytes, size - 1).contains("exceeds recorded maximum"));
    assert!(error(&bytes[..size - 1], size).contains("truncated"));
    let count_error = codec::read::<AggregateValueRecord>(&mut Cursor::new(&bytes), size)
        .err()
        .unwrap();
    assert!(count_error.to_string().contains("one value"));
    bytes[3 * size_of::<u64>()] = 0x80;
    assert!(error(&bytes, size).contains("invalid aggregate sort key flags"));
    bytes[3 * size_of::<u64>()] = 2;
    bytes.push(0);
    let length = u64::try_from(bytes.len() - size_of::<u64>()).unwrap();
    bytes[..size_of::<u64>()].copy_from_slice(&length.to_le_bytes());
    assert!(error(&bytes, bytes.len()).contains("trailing bytes"));
}

#[test]
fn sixteen_interleaved_runs_preserve_input_order_with_logarithmic_head_comparisons() {
    let mut readers = (0..16_u64)
        .map(|run| {
            let rows = (0..64)
                .map(|row| {
                    let sequence = row * 16 + run;
                    AggregateValueRecord::new(Value::Null, Vec::new(), sequence)
                })
                .collect();
            RunReader::memory(rows)
        })
        .collect::<Vec<_>>();
    let comparisons = Cell::new(0);
    let mut observed = 0;
    let order = RecordOrder::new(None, None, None);
    merge_readers(
        &mut readers,
        |left, right| {
            comparisons.set(comparisons.get() + 1);
            order.compare(left, right)
        },
        Encoding::Binary,
        |row| {
            assert_eq!(row.sequence, observed);
            observed += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(observed, 1024);
    assert!(
        comparisons.get() <= 8 * 1024 + 16,
        "{} comparisons",
        comparisons.get()
    );
}
