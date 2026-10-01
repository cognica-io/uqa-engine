//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore, StorageBackendError};

fn fixture() -> SQLiteDocumentStore {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    SQLiteDocumentStore::new(connection, "docs")
}

#[test]
fn borrowed_points_share_read_admission_and_preserve_request_order() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    for id in 1..=128 {
        store
            .put(
                id,
                BTreeMap::from([("value".into(), Value::Int(id as i64))]),
            )
            .unwrap();
    }
    store.conn.commit_transaction().unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let retained = control.memory().used();
    let transactions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&transactions);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(context.action, AuthAction::Transaction { .. }) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let ids = (1..=128).rev().chain([1, 129, 1]).collect::<Vec<_>>();
    let mut actual = Vec::new();
    assert_eq!(
        snapshot
            .for_each_fields_multi_borrowed(
                &ids,
                &["value", "missing", "value"],
                &mut |id, present, values| {
                    assert_eq!(present, id <= 128);
                    assert_eq!(
                        values[0],
                        &if present {
                            Value::Int(id as i64)
                        } else {
                            Value::Null
                        }
                    );
                    assert_eq!(values[1], &Value::Null);
                    assert!(std::ptr::eq(values[0], values[2]));
                    actual.push(id);
                    true
                },
            )
            .unwrap(),
        Some(ids.len())
    );
    assert_eq!(actual, ids);
    assert!(
        transactions.load(Ordering::Relaxed) <= 8,
        "point projection performed {} transaction commands",
        transactions.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), retained);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
}

#[test]
fn borrowed_points_keep_private_and_old_blob_values_after_rollback() {
    let mut store = fixture();
    let document = |value: i64| {
        BTreeMap::from([
            ("value".into(), Value::Int(value)),
            (
                "vector".into(),
                Value::List(vec![Value::Float(value as f64); 128]),
            ),
        ])
    };
    for id in [1, 3, 5] {
        store.put(id, document(id as i64)).unwrap();
    }
    let old = store.snapshot().unwrap();
    store.conn.begin_transaction().unwrap();
    store.put(3, document(33)).unwrap();
    store.delete(5).unwrap();
    store.put(7, document(77)).unwrap();
    let private = store.snapshot().unwrap();
    store.conn.rollback_transaction().unwrap();
    store.put(3, document(333)).unwrap();
    let current = store.snapshot().unwrap();
    for (snapshot, expected) in [
        (old, [Some(5), Some(3), Some(1), None, Some(3)]),
        (private, [None, Some(33), Some(1), Some(77), Some(33)]),
        (current, [Some(5), Some(333), Some(1), None, Some(333)]),
    ] {
        let mut position = 0;
        assert_eq!(
            snapshot
                .for_each_fields_multi_borrowed(
                    &[5, 3, 1, 7, 3],
                    &["vector", "value"],
                    &mut |_, present, values| {
                        let expected = expected[position];
                        position += 1;
                        assert_eq!(present, expected.is_some());
                        assert_eq!(values[1], &expected.map_or(Value::Null, Value::Int));
                        assert_eq!(
                            values[0],
                            &expected.map_or(Value::Null, |value| Value::List(vec![
                                Value::Float(
                                    value as f64
                                );
                                128
                            ]))
                        );
                        true
                    },
                )
                .unwrap(),
            Some(5)
        );
        assert_eq!(position, 5);
    }
}

#[test]
fn borrowed_points_stop_before_invalid_ids_and_later_payload_admission() {
    let mut store = fixture();
    store
        .put(
            1,
            BTreeMap::from([("body".into(), Value::Str("small".into()))]),
        )
        .unwrap();
    store
        .put(
            2,
            BTreeMap::from([("body".into(), Value::Str("large".repeat(1 << 18)))]),
        )
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let retained = control.memory().used();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - retained - 65536)
        .unwrap();
    let baseline = control.memory().used();
    for ids in [[1, 2], [1, u64::MAX]] {
        assert_eq!(
            snapshot
                .for_each_fields_multi_borrowed(&ids, &["body"], &mut |id, present, values| {
                    assert_eq!(id, 1);
                    assert!(present);
                    assert_eq!(values, [&Value::Str("small".into())]);
                    false
                },)
                .unwrap(),
            Some(1)
        );
    }
    assert_eq!(control.memory().used(), baseline);
    let mut visited = 0;
    assert!(matches!(
        snapshot.for_each_fields_multi_borrowed(&[1, 2], &["body"], &mut |_, _, _| {
            visited += 1;
            true
        },),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(visited, 1);
    assert_eq!(control.memory().used(), baseline);
    drop(hold);
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn borrowed_points_observe_cancellation_after_the_stopping_callback() {
    let mut store = fixture();
    store
        .put(1, BTreeMap::from([("value".into(), Value::Int(1))]))
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let retained = control.memory().used();
    let mut visited = 0;
    assert!(matches!(
        snapshot.for_each_fields_multi_borrowed(&[1, 1], &["value"], &mut |_, _, _| {
            visited += 1;
            control.cancellation().cancel();
            false
        },),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(visited, 1);
    assert_eq!(control.memory().used(), retained);
    assert!(matches!(
        snapshot.for_each_fields_multi_borrowed(&[], &["value"], &mut |_, _, _| panic!(
            "empty request"
        ),),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
}
