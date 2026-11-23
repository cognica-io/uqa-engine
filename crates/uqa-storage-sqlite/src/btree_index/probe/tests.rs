//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore};
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore};

fn fixture(values: &[(DocId, Value)]) -> (ManagedConnection, SQLiteBTreeIndexStore) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection.begin_transaction().unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    for (id, value) in values {
        documents
            .put(*id, [("key".into(), value.clone())].into())
            .unwrap();
    }
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    indexes.replace("items", &"key".into(), values).unwrap();
    connection.commit_transaction().unwrap();
    (connection, indexes)
}

#[test]
fn bit_string_scalar_and_tuple_probes_seek_without_conflating_zero_padded_lengths() {
    let bits = |bytes, length| {
        Value::BitString(uqa_core::BitStringValue::try_from_packed(bytes, length).unwrap())
    };
    for tuple in [false, true] {
        let wrap = |value| {
            if tuple {
                Value::Row(vec![Value::Str("bits".into()), value].into())
            } else {
                value
            }
        };
        for count in [64_u64, 2048] {
            let mut values = (1..=count)
                .map(|id| (id, wrap(bits(id.to_be_bytes().to_vec(), 64))))
                .collect::<Vec<_>>();
            values.push((count + 1, wrap(bits(vec![0x80], 1))));
            values.push((count + 2, wrap(bits(vec![0x80], 8))));
            let (connection, index) = fixture(&values);
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            for (target, expected) in [
                (wrap(bits(42_u64.to_be_bytes().to_vec(), 64)), vec![42]),
                (wrap(bits(vec![0x80], 1)), vec![count + 1]),
                (wrap(bits(vec![0x80], 8)), vec![count + 2]),
                (wrap(bits(vec![0x80, 0], 9)), vec![]),
            ] {
                PROBE_VM_STEPS.set(0);
                assert_eq!(
                    index.probe_equal("items", &"key".into(), &target).unwrap(),
                    Some(expected)
                );
                let steps = PROBE_VM_STEPS.get();
                assert!(
                    steps > 0 && steps < 400,
                    "{count} rows: {steps} instructions"
                );
            }
        }
    }
}

#[test]
fn cold_scalar_probes_seek_instead_of_visiting_the_posting_collection() {
    for tuple in [false, true] {
        for count in [128, 8192] {
            let values = (1..=count)
                .map(|id| {
                    (
                        id,
                        if tuple {
                            Value::Row(
                                vec![Value::Str("shared-prefix".into()), Value::Int(id as i64)]
                                    .into(),
                            )
                        } else {
                            Value::Str(format!("key-{id:08}"))
                        },
                    )
                })
                .collect::<Vec<_>>();
            let (connection, index) = fixture(&values);
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            let targets = if tuple {
                [
                    Value::Row(vec![Value::Str("shared-prefix".into()), Value::Float(42.0)].into()),
                    Value::Row(vec![Value::Str("shared-prefix".into()), Value::Int(-1)].into()),
                ]
            } else {
                [
                    Value::Str("key-00000042".into()),
                    Value::Str("missing".into()),
                ]
            };
            for target in targets {
                PROBE_VM_STEPS.set(0);
                let result = index
                    .probe_equal("items", &"key".into(), &target)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    result,
                    values
                        .iter()
                        .filter(|(_, value)| *value == target)
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                );
                let steps = PROBE_VM_STEPS.get();
                assert!(
                    steps > 0 && steps < 400,
                    "{count} rows consumed {steps} SQLite instructions"
                );
            }
        }
    }
}

#[test]
fn equality_probe_keys_and_candidates_keep_the_original_allowance_and_cancellation() {
    let control = StorageReadControl::with_limit(256);
    assert!(IndexProbe::equal(&Value::Str("x".repeat(4096)), &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let probe = IndexProbe::equal(&Value::Int(1), &control)
        .unwrap()
        .unwrap();
    assert!(control.memory().used() > 0);
    let values: Vec<_> = (1..=128).map(|id| (id, Value::Int(1))).collect();
    let (connection, _) = fixture(&values);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    assert!(connection
        .record_connection()
        .with(|sqlite| probe.read(sqlite, "items", ValueRef::Text(b"key"), &control))
        .is_err());
    drop(probe);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(IndexProbe::equal(&Value::Int(1), &control).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn equality_candidate_visitors_keep_fixed_memory_and_stop_at_the_consumer() {
    let count = 8192;
    let values: Vec<_> = (1..=count).map(|id| (id, Value::Int(1))).collect();
    let (connection, _) = fixture(&values);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let control = StorageReadControl::with_limit(256);
    let probe = IndexProbe::equal(&Value::Int(1), &control)
        .unwrap()
        .unwrap();
    let mut next = 1;
    connection
        .record_connection()
        .with(|sqlite| {
            assert!(probe.visit(
                sqlite,
                "items",
                ValueRef::Text(b"key"),
                &control,
                &mut |id| {
                    assert_eq!(id, next);
                    next += 1;
                    Ok(true)
                }
            )?);
            Ok(())
        })
        .unwrap();
    assert_eq!(next, count + 1);
    assert!(control.memory().used() <= 256);
    PROBE_VM_STEPS.set(0);
    next = 1;
    connection
        .record_connection()
        .with(|sqlite| {
            assert!(probe.visit(
                sqlite,
                "items",
                ValueRef::Text(b"key"),
                &control,
                &mut |id| {
                    assert_eq!(id, next);
                    next += 1;
                    Ok(id < 7)
                }
            )?);
            Ok(())
        })
        .unwrap();
    assert_eq!(next, 8);
    assert!(PROBE_VM_STEPS.get() < 400);
    control.cancellation().cancel();
    assert!(connection
        .record_connection()
        .with(|sqlite| {
            probe.visit(
                sqlite,
                "items",
                ValueRef::Text(b"key"),
                &control,
                &mut |_| panic!("cancelled index read delivered a candidate"),
            )
        })
        .is_err());
}

#[test]
fn native_equality_visitors_merge_private_keys_and_decline_before_delivery() {
    let (connection, index) =
        fixture(&[(1, Value::Int(1)), (3, Value::Int(1)), (5, Value::Int(2))]);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let check = |expected: &[DocId]| {
        let mut found = Vec::new();
        assert!(index
            .visit_equal("items", &"key".into(), &Value::Int(1), &mut |id| {
                found.push(id);
                true
            })
            .unwrap());
        assert_eq!(found, expected);
    };
    check(&[1, 3]);
    connection.begin_transaction().unwrap();
    connection.savepoint("original").unwrap();
    index
        .apply_write("items", 1, Some(&[("key".into(), Value::Int(2))].into()))
        .unwrap();
    index
        .apply_write("items", 5, Some(&[("key".into(), Value::Int(1))].into()))
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    documents
        .put(2, [("key".into(), Value::Int(1))].into())
        .unwrap();
    index
        .apply_write("items", 2, Some(&[("key".into(), Value::Int(1))].into()))
        .unwrap();
    check(&[2, 3, 5]);
    let mut found = Vec::new();
    assert!(index
        .visit_equal("items", &"key".into(), &Value::Int(1), &mut |id| {
            found.push(id);
            false
        })
        .unwrap());
    assert_eq!(found, [2]);
    index
        .apply_write(
            "items",
            3,
            Some(&[("key".into(), Value::FixedChar("x".into()))].into()),
        )
        .unwrap();
    assert!(!index
        .visit_equal("items", &"key".into(), &Value::Int(1), &mut |_| {
            panic!("unsupported private comparison delivered a partial result")
        })
        .unwrap());
    connection.rollback_to_savepoint("original").unwrap();
    check(&[1, 3]);
    connection.rollback_transaction().unwrap();
    check(&[1, 3]);
}

#[test]
fn scalar_probes_preserve_cross_numeric_equality_and_signed_zero() {
    let values = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(0),
        Value::Int(1),
        Value::Int(i64::MAX),
        Value::Int(9_007_199_254_740_993),
        Value::Float(-0.0),
        Value::Float(0.0),
        Value::Float(1.0),
        Value::Float(i64::MAX as f64),
        Value::Float(9_007_199_254_740_992.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Str("a\0\"\\b".into()),
        Value::Bytes(vec![0, 255]),
        Value::Row(vec![Value::Str("key".into()), Value::Int(1)].into()),
        Value::Row(vec![Value::Str("key".into()), Value::Float(1.0)].into()),
        Value::Row(vec![Value::Str("key".into()), Value::Bool(true)].into()),
        Value::Row(vec![Value::Null, Value::Int(0)].into()),
        Value::Row(vec![Value::Null, Value::Float(-0.0)].into()),
        Value::Row(vec![Value::Row(vec![Value::Int(1)].into())].into()),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, value)| (id as u64 + 1, value))
    .collect::<Vec<_>>();
    let (connection, index) = fixture(&values);
    assert!(index
        .probe_equal("items", &"key".into(), &Value::Int(1))
        .unwrap()
        .is_none());
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    for (_, target) in &values {
        assert_eq!(
            index.probe_equal("items", &"key".into(), target).unwrap(),
            Some(
                values
                    .iter()
                    .filter(|(_, value)| value == target)
                    .map(|(id, _)| *id)
                    .collect()
            )
        );
    }
}

#[test]
fn private_scalar_changes_savepoints_deletes_and_old_snapshots_are_not_confused() {
    let (connection, index) = fixture(&[(1, Value::Int(1)), (2, Value::Int(2)), (3, Value::Null)]);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let other = connection.new_session();
    let old = SQLiteBTreeIndexStore::new(other.clone());
    other.begin_deferred_transaction().unwrap();
    assert_eq!(
        old.probe_equal("items", &"key".into(), &Value::Int(1))
            .unwrap(),
        Some(vec![1])
    );
    connection.begin_transaction().unwrap();
    index
        .apply_write("items", 1, Some(&[("key".into(), Value::Int(2))].into()))
        .unwrap();
    assert_eq!(
        index
            .probe_equal("items", &"key".into(), &Value::Int(1))
            .unwrap(),
        Some(vec![])
    );
    assert_eq!(
        index
            .probe_equal("items", &"key".into(), &Value::Int(2))
            .unwrap(),
        Some(vec![1, 2])
    );
    connection.savepoint("retain").unwrap();
    index.apply_write("items", 2, None).unwrap();
    SQLiteDocumentStore::new(connection.clone(), "items")
        .delete(2)
        .unwrap();
    assert_eq!(
        index
            .probe_equal("items", &"key".into(), &Value::Int(2))
            .unwrap(),
        Some(vec![1])
    );
    connection.rollback_to_savepoint("retain").unwrap();
    assert_eq!(
        index
            .probe_equal("items", &"key".into(), &Value::Int(2))
            .unwrap(),
        Some(vec![1, 2])
    );
    connection.commit_transaction().unwrap();
    // A historical view must never answer from a newer physical projection.
    assert!(old
        .probe_equal("items", &"key".into(), &Value::Int(1))
        .unwrap()
        .is_none());
    assert_eq!(
        old.load("items", &"key".into()).unwrap().unwrap()[0],
        (1, Value::Int(1))
    );
    other.rollback_transaction().unwrap();
    assert_eq!(
        old.probe_equal("items", &"key".into(), &Value::Int(2))
            .unwrap(),
        Some(vec![1, 2])
    );
}

#[test]
fn unsupported_comparisons_and_unbuilt_indexes_decline_without_incorrect_candidates() {
    let (connection, index) = fixture(&[
        (1, Value::Int(1)),
        (2, Value::Row(vec![Value::FixedChar("value".into())].into())),
    ]);
    for native in [false, true] {
        if native {
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
        }
        assert_eq!(
            index
                .probe_equal("items", &"key".into(), &Value::Int(1))
                .unwrap(),
            None
        );
        assert_eq!(
            index
                .probe_equal("items", &"key".into(), &Value::Null)
                .unwrap(),
            None
        );
        assert_eq!(
            index
                .probe_equal("items", &"absent".into(), &Value::Int(1))
                .unwrap(),
            None
        );
    }
}

#[test]
fn incomplete_native_index_support_declines_until_repaired_and_keeps_undo() {
    let (connection, index) = fixture(&[(1, Value::Int(1)), (2, Value::Int(2))]);
    // A legacy writer omitted an entry while retaining its document.
    index.apply_write("items", 2, None).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let probe = || {
        index
            .probe_equal("items", &"key".into(), &Value::Int(2))
            .unwrap()
    };
    assert_eq!(probe(), None);
    index
        .apply_write("items", 2, Some(&[("key".into(), Value::Int(2))].into()))
        .unwrap();
    assert_eq!(probe(), Some(vec![2]));
    connection.begin_transaction().unwrap();
    connection.savepoint("complete").unwrap();
    index.apply_write("items", 2, None).unwrap();
    assert_eq!(probe(), None);
    connection.rollback_to_savepoint("complete").unwrap();
    assert_eq!(probe(), Some(vec![2]));
    SQLiteDocumentStore::new(connection.clone(), "items")
        .put(3, [("key".into(), Value::Int(3))].into())
        .unwrap();
    assert_eq!(probe(), None);
    connection.rollback_transaction().unwrap();
    assert_eq!(probe(), Some(vec![2]));
}

fn within(value: &Value, low: Bound<&Value>, high: Bound<&Value>) -> bool {
    (match low {
        Bound::Unbounded => true,
        Bound::Included(low) => value >= low,
        Bound::Excluded(low) => value > low,
    }) && (match high {
        Bound::Unbounded => true,
        Bound::Included(high) => value <= high,
        Bound::Excluded(high) => value < high,
    })
}

#[test]
fn a_range_probe_selects_what_core_orders_between_its_ends() {
    let mut values = Vec::new();
    let mut add = |value: Value| values.push((values.len() as DocId + 1, value));
    for number in -20..=20 {
        add(Value::Int(number * 3));
    }
    for number in -8..=8 {
        add(Value::Float(f64::from(number) * 2.5));
    }
    add(Value::Bool(true));
    add(Value::Bool(false));
    for text in ["", "a", "ab", "b", "m", "z"] {
        add(Value::Str(text.into()));
    }
    for _ in 0..3 {
        add(Value::Null);
    }
    add(Value::Bytes(vec![1, 2]));
    let (connection, index) = fixture(&values);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let ends = [
        Value::Null,
        Value::Int(-30),
        Value::Int(-7),
        Value::Float(-7.5),
        Value::Int(0),
        Value::Float(2.5),
        Value::Bool(true),
        Value::Int(60),
        Value::Float(1e300),
        Value::Str("a".into()),
        Value::Str("m".into()),
        Value::Bytes(vec![1]),
    ];
    let mut bounds = vec![Bound::Unbounded];
    for end in &ends {
        bounds.extend([Bound::Included(end), Bound::Excluded(end)]);
    }
    for &low in &bounds {
        for &high in &bounds {
            let expected: Vec<DocId> = values
                .iter()
                .filter(|(_, value)| within(value, low, high))
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(
                index
                    .probe_range("items", &"key".into(), low, high)
                    .unwrap(),
                Some(expected),
                "{low:?} to {high:?}"
            );
        }
    }
    // An end outside the domain of the keys has no interval.
    let fixed = Value::FixedChar("a".into());
    assert_eq!(
        index
            .probe_range(
                "items",
                &"key".into(),
                Bound::Included(&fixed),
                Bound::Unbounded
            )
            .unwrap(),
        None
    );
}

#[test]
fn a_range_probe_seeks_its_interval_whatever_the_index_holds() {
    let mut steps = Vec::new();
    for count in [64_u64, 4096] {
        let values: Vec<_> = (1..=count)
            .map(|id| (id, Value::Int(i64::try_from(id).unwrap())))
            .collect();
        let (connection, index) = fixture(&values);
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        PROBE_VM_STEPS.set(0);
        assert_eq!(
            index
                .probe_range(
                    "items",
                    &"key".into(),
                    Bound::Excluded(&Value::Int(20)),
                    Bound::Included(&Value::Int(30)),
                )
                .unwrap(),
            Some((21..=30).collect())
        );
        steps.push(PROBE_VM_STEPS.get());
    }
    assert!(
        steps[0] > 0 && steps[1] == steps[0],
        "ten postings of 64 and of 4,096 consumed {steps:?} SQLite instructions"
    );
}

#[test]
fn a_range_probe_merges_private_changes_and_declines_what_it_cannot_order() {
    let (connection, index) = fixture(&[
        (1, Value::Int(10)),
        (2, Value::Int(20)),
        (3, Value::Int(30)),
        (4, Value::Int(40)),
        (5, Value::Null),
    ]);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let (low, high) = (Value::Int(15), Value::Int(40));
    let range = |index: &SQLiteBTreeIndexStore| {
        index
            .probe_range(
                "items",
                &"key".into(),
                Bound::Included(&low),
                Bound::Excluded(&high),
            )
            .unwrap()
    };
    assert_eq!(range(&index), Some(vec![2, 3]));
    let other = connection.new_session();
    let old = SQLiteBTreeIndexStore::new(other.clone());
    other.begin_deferred_transaction().unwrap();
    assert_eq!(range(&old), Some(vec![2, 3]));
    connection.begin_transaction().unwrap();
    connection.savepoint("original").unwrap();
    let write = |id, value: Value| {
        index
            .apply_write("items", id, Some(&[("key".into(), value)].into()))
            .unwrap();
    };
    // One row leaves the interval, one enters it, one is new at its inclusive end and one is deleted.
    write(2, Value::Int(50));
    write(4, Value::Int(35));
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    documents
        .put(6, [("key".into(), Value::Float(15.0))].into())
        .unwrap();
    write(6, Value::Float(15.0));
    index.apply_write("items", 3, None).unwrap();
    documents.delete(3).unwrap();
    assert_eq!(range(&index), Some(vec![4, 6]));
    // A private value without a key leaves the index unable to answer.
    write(1, Value::FixedChar("x".into()));
    assert_eq!(range(&index), None);
    connection.rollback_to_savepoint("original").unwrap();
    assert_eq!(range(&index), Some(vec![2, 3]));
    write(2, Value::Int(50));
    connection.commit_transaction().unwrap();
    assert_eq!(range(&index), Some(vec![3]));
    // A historical view must never answer from a newer physical projection.
    assert_eq!(range(&old), None);
    other.rollback_transaction().unwrap();
    assert_eq!(range(&old), Some(vec![3]));
}

#[test]
fn a_range_probe_declines_postings_beyond_the_session_allowance() {
    let count = 20_000_u64;
    let values: Vec<_> = (1..=count)
        .map(|id| (id, Value::Int(i64::try_from(id).unwrap())))
        .collect();
    let (connection, index) = fixture(&values);
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 64 * 1024,
            ..VersionedSessionOptions::default()
        })
        .unwrap();
    // 20,000 identities need 160,000 bytes: the caller answers without the index.
    assert_eq!(
        index
            .probe_range("items", &"key".into(), Bound::Unbounded, Bound::Unbounded)
            .unwrap(),
        None
    );
    assert_eq!(
        index
            .probe_range(
                "items",
                &"key".into(),
                Bound::Included(&Value::Int(19_999)),
                Bound::Unbounded,
            )
            .unwrap(),
        Some(vec![19_999, 20_000])
    );
}
