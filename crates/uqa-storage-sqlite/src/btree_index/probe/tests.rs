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
    assert!(EqualityProbe::new(&Value::Str("x".repeat(4096)), &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let probe = EqualityProbe::new(&Value::Int(1), &control)
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
    assert!(EqualityProbe::new(&Value::Int(1), &control).is_err());
    assert_eq!(control.memory().used(), 0);
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
