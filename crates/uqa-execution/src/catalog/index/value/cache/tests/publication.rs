//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use uqa_storage::ValueIndexKey;

#[test]
fn retained_index_cache_prepares_all_fields_before_publishing_a_row() {
    let a = ValueIndexKey::Column("a".into());
    let b = ValueIndexKey::Column("b".into());
    let control = StorageReadControl::with_limit(64 * 1024);
    for allowance in [0, 64 * 1024] {
        let mut indexes = BTreeMap::from([
            (a.clone(), build(&[(1, Value::Int(10))], false, allowance)),
            (b.clone(), build(&[(1, Value::Int(20))], true, 64 * 1024)),
        ]);
        let old = indexes.clone();
        let new = BTreeMap::from([
            (a.clone(), Value::Int(30)),
            (b.clone(), Value::List(vec![Value::Int(40)])),
        ]);
        let error = apply_cached_values(
            &mut indexes,
            1,
            Some(&new),
            &StorageReadControl::with_limit(0),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        for field in [&a, &b] {
            assert_eq!(
                indexes[field].stored_value(1, &control).unwrap(),
                old[field].stored_value(1, &control).unwrap()
            );
        }
        apply_cached_values(&mut indexes, 1, Some(&new), &control).unwrap();
        for field in [&a, &b] {
            assert_eq!(
                indexes[field].stored_value(1, &control).unwrap().as_deref(),
                new.get(field)
            );
        }
        apply_cached_values(&mut indexes, 1, None, &control).unwrap();
        for field in [&a, &b] {
            assert!(!indexes[field].contains(1, &control).unwrap());
            assert!(old[field].contains(1, &control).unwrap());
        }
    }
}

#[test]
fn retained_index_projection_keeps_null_presence_early_stop_and_cancellation() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let first = build(&[(1, Value::Int(1)), (2, Value::Null)], false, 0);
    let second = build(&[(1, Value::Str("stored".into()))], true, 64 * 1024);
    let mut seen = Vec::new();
    assert_eq!(
        visit_stored_fields(
            &[&first, &second],
            &first,
            &[1, 2, 3],
            &control,
            &mut |id, exists, values| {
                seen.push((
                    id,
                    exists,
                    values
                        .iter()
                        .map(|value| (*value).clone())
                        .collect::<Vec<_>>(),
                ));
                true
            },
        )
        .unwrap(),
        3
    );
    assert_eq!(
        seen,
        [
            (1, true, vec![Value::Int(1), Value::Str("stored".into())]),
            (2, false, vec![]),
            (3, false, vec![]),
        ]
    );
    seen.clear();
    assert_eq!(
        visit_stored_fields(&[], &first, &[2, 3], &control, &mut |id, exists, values| {
            assert_eq!((id, exists, values.len()), (2, true, 0));
            false
        })
        .unwrap(),
        1
    );
    control.cancellation().cancel();
    assert_eq!(
        visit_stored_fields(&[&first], &first, &[1], &control, &mut |_, _, _| {
            panic!("cancelled projection must not visit");
        })
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
}
