//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

fn ordered(index: &RetainedValueIndex, control: &StorageReadControl) -> Vec<(Value, DocId)> {
    let mut result = Vec::new();
    index
        .visit_range(Bound::Unbounded, Bound::Unbounded, control, &mut |id| {
            result.push((index.get(id, control)?.unwrap(), id));
            Ok(true)
        })
        .unwrap();
    result
}

fn height(index: &RetainedValueIndex, id: Option<DocId>, control: &StorageReadControl) -> u8 {
    let Some(id) = id else { return 0 };
    let records = Records::new(&index.records, &index.memory, control);
    let node = records.node(id).unwrap();
    let left = height(index, node.left, control);
    let right = height(index, node.right, control);
    assert!(left.abs_diff(right) <= 1);
    assert_eq!(node.height, left.max(right) + 1);
    node.height
}

fn validate(
    index: &RetainedValueIndex,
    expected: &BTreeMap<DocId, Value>,
    control: &StorageReadControl,
) {
    let mut sorted = expected
        .iter()
        .map(|(id, value)| (value.clone(), *id))
        .collect::<Vec<_>>();
    sorted.sort();
    assert_eq!(ordered(index, control), sorted);
    assert_eq!(index.len(), expected.len());
    let mut by_id = Vec::new();
    index
        .visit_values(control, &mut |id, value| {
            by_id.push((id, value));
            Ok(true)
        })
        .unwrap();
    assert_eq!(
        by_id,
        expected
            .iter()
            .map(|(id, value)| (*id, value.clone()))
            .collect::<Vec<_>>()
    );
    height(index, index.root, control);
}

#[test]
fn retained_value_index_matches_ordered_replacements_and_independent_snapshots() {
    let control = StorageReadControl::with_limit(64 * 1024);
    for allowance in [0, 64 * 1024] {
        let mut index = RetainedValueIndex::new(allowance);
        let mut expected = BTreeMap::new();
        let mut saved = None;
        for step in 0..160_u64 {
            let id = step.wrapping_mul(53) % 41;
            let value = (step % 5 != 0).then_some(Value::Int((step % 11) as i64));
            index.replace(id, value.as_ref(), &control).unwrap();
            if let Some(value) = value {
                expected.insert(id, value);
            } else {
                expected.remove(&id);
            }
            if step == 79 {
                saved = Some((index.clone(), expected.clone()));
            }
            if step % 17 == 0 {
                validate(&index, &expected, &control);
            }
        }
        validate(&index, &expected, &control);
        let (saved, earlier) = saved.unwrap();
        validate(&saved, &earlier, &control);
        index.clear();
        assert!(index.is_empty());
        assert!(ordered(&index, &control).is_empty());
        validate(&saved, &earlier, &control);
    }
}

#[test]
fn retained_value_index_keeps_exact_carriers_and_full_width_identities() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = RetainedValueIndex::new(64 * 1024);
    let values = [
        Value::Int(1),
        Value::Float(1.0),
        Value::Bool(true),
        Value::Float(-0.0),
        Value::Null,
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Str("한글🦀".repeat(2048)),
        Value::Bytes(vec![0, 255, 1]),
        Value::Decimal(uqa_core::DecimalValue::parse("1.0000").unwrap()),
    ];
    for (ordinal, value) in values.iter().enumerate() {
        index
            .replace(u64::MAX - ordinal as u64, Some(value), &control)
            .unwrap();
    }
    for (ordinal, value) in values.iter().enumerate() {
        assert!(index
            .get(u64::MAX - ordinal as u64, &control)
            .unwrap()
            .unwrap()
            .has_same_representation(value));
    }
    let mut equal = Vec::new();
    let one = Value::Int(1);
    index
        .visit_range(
            Bound::Included(&one),
            Bound::Included(&one),
            &control,
            &mut |id| {
                equal.push(id);
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(
        equal,
        vec![u64::MAX - 9, u64::MAX - 2, u64::MAX - 1, u64::MAX]
    );
    let mut middle = Vec::new();
    index
        .visit_range(
            Bound::Excluded(&Value::Int(0)),
            Bound::Excluded(&Value::Int(2)),
            &control,
            &mut |id| {
                middle.push(id);
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(middle, equal);
    assert_eq!(
        index
            .count_range(
                Bound::Excluded(&Value::Int(0)),
                Bound::Excluded(&Value::Int(2)),
                &control
            )
            .unwrap(),
        equal.len()
    );
}

#[test]
fn retained_value_index_bounds_large_keys_and_repeated_equality_work() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = None;
    let allocation = allocation_counter::measure(|| {
        let mut rows =
            (0..256_u64).map(|id| (id, Value::Str(format!("{id:08}{}", "x".repeat(8192)))));
        index = Some(
            RetainedValueIndex::from_ordered(64 * 1024, 256, &mut || Ok(rows.next()), &control)
                .unwrap(),
        );
    });
    let index = index.unwrap();
    assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
    records::VALUE_READS.with(|reads| reads.set(0));
    assert_eq!(
        index
            .count_range(Bound::Unbounded, Bound::Unbounded, &control)
            .unwrap(),
        256
    );
    assert_eq!(records::VALUE_READS.with(std::cell::Cell::get), 0);
    let key = Value::Str(format!("{:08}{}", 123, "x".repeat(8192)));
    for _ in 0..32 {
        let mut ids = Vec::new();
        index
            .visit_range(
                Bound::Included(&key),
                Bound::Included(&key),
                &control,
                &mut |id| {
                    ids.push(id);
                    Ok(true)
                },
            )
            .unwrap();
        assert_eq!(ids, [123]);
    }
    let reads = records::VALUE_READS.with(std::cell::Cell::get);
    assert!(
        reads <= 32 * 20,
        "{reads} decoded keys for 32 equality probes"
    );
}

#[test]
fn retained_value_index_failure_and_early_stop_preserve_prior_roots() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = RetainedValueIndex::new(64 * 1024);
    for id in 0..32 {
        index.replace(id, Some(&Value::Int(7)), &control).unwrap();
    }
    let before = index.clone();
    let hold = index
        .memory
        .reserve(index.memory.limit() - index.memory.used())
        .unwrap();
    assert!(index.replace(10, Some(&Value::Int(11)), &control).is_err());
    drop(hold);
    assert_eq!(ordered(&index, &control), ordered(&before, &control));
    let mut ids = Vec::new();
    index
        .visit_range(Bound::Unbounded, Bound::Unbounded, &control, &mut |id| {
            ids.push(id);
            Ok(ids.len() < 7)
        })
        .unwrap();
    assert_eq!(ids, (0..7).collect::<Vec<_>>());
    let error = index
        .visit_values(&control, &mut |_, _| {
            Err(crate::StorageBackendError::Other(
                "consumer sentinel".into(),
            ))
        })
        .unwrap_err();
    assert!(error.to_string().contains("consumer sentinel"));
    control.cancellation().cancel();
    assert!(index.replace(4, None, &control).is_err());
    control.cancellation().reset();
    assert_eq!(ordered(&index, &control), ordered(&before, &control));
}

#[test]
fn retained_value_index_ordered_build_rejects_invalid_sources_without_publishing() {
    let control = StorageReadControl::with_limit(64 * 1024);
    for (count, rows) in [
        (2, vec![(1, Value::Int(2))]),
        (1, vec![(1, Value::Int(2)), (2, Value::Int(3))]),
        (2, vec![(1, Value::Int(2)), (2, Value::Int(1))]),
        (2, vec![(1, Value::Int(2)), (1, Value::Int(3))]),
    ] {
        let mut rows = rows.into_iter();
        assert!(RetainedValueIndex::from_ordered(
            64 * 1024,
            count,
            &mut || Ok(rows.next()),
            &control
        )
        .is_err());
    }
    let mut rows = (0..63).map(|id| (id, Value::Int((id / 3) as i64)));
    let index =
        RetainedValueIndex::from_ordered(64 * 1024, 63, &mut || Ok(rows.next()), &control).unwrap();
    for low in 0..21 {
        for high in low..21 {
            assert_eq!(
                index
                    .count_range(
                        Bound::Included(&Value::Int(low)),
                        Bound::Excluded(&Value::Int(high)),
                        &control
                    )
                    .unwrap(),
                (high - low) as usize * 3
            );
        }
    }
    assert_eq!(index.len(), 63);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn retained_value_index_rotations_preserve_model_after_arbitrary_edits(
        edits in proptest::collection::vec((0_u64..64, proptest::option::of(-99_i64..99)), 1..128)
    ) {
        let control = StorageReadControl::with_limit(4 * 1024 * 1024);
        let mut index = RetainedValueIndex::new(4 * 1024 * 1024);
        let mut expected = BTreeMap::new();
        for (id, value) in edits {
            let value = value.map(Value::Int);
            index.replace(id, value.as_ref(), &control).unwrap();
            if let Some(value) = value { expected.insert(id, value); } else { expected.remove(&id); }
        }
        validate(&index, &expected, &control);
        for bound in [-100, -50, 0, 50, 100] {
            let bound = Value::Int(bound);
            let expected = expected.values().filter(|value| **value <= bound).count();
            proptest::prop_assert_eq!(index.count_range(Bound::Unbounded, Bound::Included(&bound), &control).unwrap(), expected);
        }
    }
}

#[test]
fn retained_value_index_rejects_invalid_chunk_lengths_before_allocation() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = RetainedValueIndex::new(64 * 1024);
    index.replace(0, Some(&Value::Int(42)), &control).unwrap();
    let records = Records::new(&index.records, &index.memory, &control);
    let mut node = records.node(0).unwrap();
    node.bytes = u64::MAX;
    records.set_node(0, node).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = index.get(0, &control).unwrap_err();
        assert!(error.to_string().contains("value"));
    });
    assert!(allocation.bytes_max < 128 * 1024, "{allocation:?}");
}
