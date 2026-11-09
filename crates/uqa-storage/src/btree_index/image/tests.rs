//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, Predicate};

fn production(control: &StorageReadControl) -> ProductionControl<'_> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
}

fn field(control: &StorageReadControl) -> Budgeted<String> {
    production(control)
        .copy_text("key")
        .unwrap()
        .into_budgeted()
        .unwrap()
}

fn key(value: &Value, control: &StorageReadControl) -> Budgeted<Value> {
    production(control)
        .copy_value(value)
        .unwrap()
        .into_budgeted()
        .unwrap()
}

#[test]
fn admitted_btree_keys_match_native_ranges_and_keep_original_input_until_retirement() {
    let input = StorageReadControl::with_limit(1 << 20);
    let field = field(&input);
    let field_pointer = field.as_ptr();
    let mut imported = BTreeIndexImageBuilder::new(field, &input).unwrap();
    let mut native = BTreeIndex::new("key");
    for (id, value) in [
        (0, Value::Int(-3)),
        (7, Value::Int(5)),
        (11, Value::Int(5)),
        (29, Value::Str("original".into())),
        (u64::MAX, Value::Null),
    ] {
        let owned = key(&value, &input);
        let pointer = match &*owned {
            Value::Str(text) => Some(text.as_ptr()),
            _ => None,
        };
        imported = imported.add_value(id, owned).unwrap();
        if let Some(pointer) = pointer {
            let (Value::Str(stored), _) = imported.index.entries.get_key_value(&value).unwrap()
            else {
                panic!("stored text key")
            };
            assert_eq!(stored.as_ptr(), pointer);
        }
        native.insert(id, value);
    }
    let mut imported = imported.finish().unwrap();
    assert_eq!(imported.field().as_ptr(), field_pointer);
    for predicate in [
        Predicate::Equals(Value::Int(5)),
        Predicate::GreaterThan(Value::Int(-3)),
        Predicate::GreaterThanOrEqual(Value::Int(5)),
        Predicate::LessThan(Value::Int(5)),
        Predicate::LessThanOrEqual(Value::Int(5)),
        Predicate::Between {
            low: Value::Int(5),
            high: Value::Int(-3),
        },
        Predicate::Between {
            low: Value::Int(-3),
            high: Value::Int(5),
        },
        Predicate::IsNull,
        Predicate::IsNotNull,
        Predicate::InSet(
            [Value::Int(5), Value::Str("original".into())]
                .into_iter()
                .collect(),
        ),
    ] {
        assert_eq!(
            imported.scan(&predicate),
            native.scan(&predicate),
            "{predicate:?}"
        );
        assert_eq!(
            imported.estimate_cardinality(&predicate),
            native.estimate_cardinality(&predicate)
        );
        let mut actual = vec![];
        let mut expected = vec![];
        imported.visit_matching_ids(&predicate, &mut |id| {
            actual.push(id);
            true
        });
        native.visit_matching_ids(&predicate, &mut |id| {
            expected.push(id);
            true
        });
        assert_eq!(actual, expected);
    }
    let mut copied = imported.clone();
    input.cancellation().cancel();
    imported.insert(8, Value::Int(5));
    imported.remove(7, &Value::Int(5));
    assert_eq!(imported.equality_ids(&Value::Int(5)), &[8, 11]);
    assert_eq!(copied.equality_ids(&Value::Int(5)), &[7, 11]);
    drop(imported);
    assert!(input.memory().used() > 0);
    copied.insert(99, Value::Int(5));
    assert_eq!(copied.equality_ids(&Value::Int(5)), &[7, 11, 99]);
    drop(copied);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn cloned_admitted_btree_keeps_shared_opaque_key_payloads_after_original_retirement() {
    let input = StorageReadControl::with_limit(8 << 20);
    let value = Value::Datum(uqa_core::DatumValue::new(25, u32::MAX, vec![7; 2 << 20]));
    let original = BTreeIndexImageBuilder::new(field(&input), &input)
        .unwrap()
        .add_value(7, key(&value, &input))
        .unwrap()
        .finish()
        .unwrap();
    let mut cloned = original.clone();
    input.cancellation().cancel();
    drop(original);
    assert!(input.memory().used() >= 2 << 20);
    assert_eq!(cloned.equality_ids(&value), [7]);
    cloned.insert(9, value.clone());
    assert_eq!(cloned.equality_ids(&value), [7, 9]);
    assert!(input.memory().used() >= 2 << 20);
    cloned.remove(7, &value);
    cloned.remove(9, &value);
    assert!(input.memory().used() < 2 << 20);
    drop(cloned);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn admitted_btree_rejection_retires_unfunded_foreign_unordered_and_cancelled_input() {
    for failure in 0..5 {
        let input = StorageReadControl::with_limit(4096);
        let other = StorageReadControl::with_limit(4096);
        let index = BTreeIndexImageBuilder::new(field(&input), &input)
            .unwrap()
            .add_value(7, key(&Value::Int(1), &input))
            .unwrap();
        let value = match failure {
            0 => key(&Value::Int(2), &other),
            1 => Budgeted::new(
                Value::Str("unfunded".into()),
                input.memory().empty_reservation(),
            ),
            2 => key(&Value::Int(1), &input),
            _ => key(&Value::Int(2), &input),
        };
        let occupied =
            (failure == 3).then(|| input.memory().reserve(input.memory().available()).unwrap());
        if failure == 4 {
            input.cancellation().cancel();
        }
        assert!(index.add_value(7, value).is_err());
        assert_eq!(other.memory().used(), 0);
        assert_eq!(
            input.memory().used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        assert_eq!(input.memory().used(), 0);
    }
}
