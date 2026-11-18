//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ArrayValue, DatumValue, LegacyVectorKind, LegacyVectorValue, RecordValue};

fn nested(depth: usize) -> Value {
    let mut value = Value::Str("leaf".into());
    for level in 0..depth {
        value = match level % 5 {
            0 => Value::List(vec![value, Value::Str("sibling".into())]),
            1 => Value::Map([("first".into(), value), ("last".into(), Value::Null)].into()),
            2 => Value::Row(vec![value].into()),
            3 => Value::Record(vec![("field".into(), value)].into()),
            // The preceding named record is one atomic array element.
            _ => Value::Array(ArrayValue::try_new(vec![value]).unwrap()),
        };
    }
    value
}

fn datum(width: usize) -> Value {
    Value::Datum(
        DatumValue::new(65_536, u32::MAX, vec![7]).with_record_fields(
            (0..width)
                .map(|index| RecordDatumField {
                    name: format!("field_{index}"),
                    type_oid: 25,
                    type_modifier: -1,
                    collation_oid: None,
                    length: -1,
                    alignment: 4,
                    dropped: index % 2 == 0,
                })
                .collect(),
        ),
    )
}

#[test]
fn native_value_retirement_releases_deep_mixed_trees_without_stack_growth_or_allocation() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for prefix in [0, 1, 8192, 20_000] {
                let value = nested(8192);
                let mut retirement = ValueRetirement::new(value);
                let released = allocation_counter::measure(|| {
                    for _ in 0..prefix {
                        assert!(!retirement.advance());
                    }
                    drop(retirement);
                });
                assert_eq!(released.count_total, 0);
                assert!(released.count_current < -8192);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_value_retirement_bounds_wide_container_and_descriptor_steps() {
    const WIDTH: usize = 4096;
    for value in [
        Value::Record(RecordValue::new(
            (0..WIDTH)
                .map(|i| (format!("field_{i}"), Value::Str("payload".into())))
                .collect(),
        )),
        datum(WIDTH),
        Value::Map(
            (0..WIDTH)
                .map(|i| (format!("key_{i:04}"), Value::Str("payload".into())))
                .collect(),
        ),
        Value::LegacyVector(
            LegacyVectorValue::try_new(LegacyVectorKind::Oid, vec![Value::Int(1); WIDTH]).unwrap(),
        ),
    ] {
        let mut retirement = ValueRetirement::new(value);
        let mut steps = 0;
        let mut done = false;
        while !done {
            let released = allocation_counter::measure(|| done = retirement.advance());
            assert_eq!(released.count_total, 0);
            assert!(released.count_current >= -16, "{released:?}");
            steps += 1;
            assert!(steps < 8 * WIDTH);
        }
        assert!(steps >= WIDTH);
        assert!(retirement.advance());
        assert_eq!(
            allocation_counter::measure(|| drop(retirement)).count_total,
            0
        );
    }
}

#[test]
fn native_value_retirement_preserves_independently_retained_descriptors() {
    let record = RecordValue::from_parts(vec![("kept".into(), nested(16))], Some(65_536));
    let descriptor = record.descriptor().clone();
    let retained = RecordValue::from_shared_parts(vec![Value::Int(42)], descriptor).unwrap();
    drop(ValueRetirement::new(Value::Record(record)));
    assert_eq!(retained.descriptor().field_names(), &["kept"]);
    assert_eq!(retained.descriptor().type_oid(), Some(65_536));
    assert_eq!(retained.values(), &[Value::Int(42)]);
    drop(ValueRetirement::new(Value::Record(retained)));

    let value = datum(7);
    let reader = value.clone();
    drop(ValueRetirement::new(value));
    let Value::Datum(reader) = reader else {
        unreachable!()
    };
    assert_eq!(reader.bytes(), &[7]);
    assert_eq!(reader.offset(), u32::MAX);
    assert_eq!(reader.record_fields().unwrap()[6].name, "field_6");
    drop(ValueRetirement::new(Value::Datum(reader)));
}

#[test]
fn native_value_retirement_transfers_shared_metadata_to_one_final_owner() {
    const WIDTH: usize = 4096;
    let value = datum(WIDTH);
    let other = value.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [value, other]
        .into_iter()
        .map(|value| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut retirement = ValueRetirement::new(value);
                let mut steps = 0;
                barrier.wait();
                let released = allocation_counter::measure(|| {
                    while !retirement.advance() {
                        steps += 1;
                    }
                    drop(retirement);
                });
                assert_eq!(released.count_total, 0);
                steps
            })
        })
        .collect();
    let steps: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(steps.iter().filter(|&&steps| steps >= WIDTH).count(), 1);
}

#[test]
fn native_value_retirement_finishes_every_suspended_prefix_without_leaking() {
    for prefix in 0..256 {
        let mut complete = false;
        let total = allocation_counter::measure(|| {
            let mut value = nested(13);
            let mut retirement = ValueRetirement::take(&mut value);
            assert!(matches!(value, Value::Null));
            for _ in 0..prefix {
                if retirement.advance() {
                    complete = true;
                    break;
                }
            }
            assert_eq!(
                allocation_counter::measure(|| drop(retirement)).count_total,
                0
            );
        });
        assert_eq!(total.count_current, 0, "prefix {prefix}: {total:?}");
        assert_eq!(total.bytes_current, 0, "prefix {prefix}: {total:?}");
        if complete {
            assert!(prefix > 13);
            return;
        }
    }
    panic!("retirement did not finish");
}
