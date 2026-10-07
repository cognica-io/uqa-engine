//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consecutive indexed results use a bounded cursor without changing point-read semantics.

use super::tests::fixture;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use uqa_core::Value;
use uqa_storage::DocumentStore;

fn document(value: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("value".into(), Value::Int(value)),
        (
            "vector".into(),
            Value::List(vec![Value::Float(value as f64); 128]),
        ),
    ])
}

#[test]
fn consecutive_points_do_not_prepare_one_statement_per_row() {
    for count in [32, 128] {
        let mut store = fixture();
        store.conn.begin_transaction().unwrap();
        for id in 1..=count {
            store
                .put(
                    id,
                    BTreeMap::from([("value".into(), Value::Int(id as i64))]),
                )
                .unwrap();
        }
        store.conn.commit_transaction().unwrap();
        let snapshot = store.snapshot().unwrap();
        let selects = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&selects);
        store
            .conn
            .with_physical(|sqlite| {
                sqlite.set_prepared_statement_cache_capacity(0);
                sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                    if matches!(context.action, AuthAction::Select) {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    Authorization::Allow
                }))?;
                Ok(())
            })
            .unwrap();
        let ids = (1..=count).collect::<Vec<_>>();
        let mut visited = 0;
        assert_eq!(
            snapshot
                .for_each_fields_multi_borrowed(&ids, &["value"], &mut |id, present, values| {
                    assert!(present);
                    assert_eq!(values, [&Value::Int(id as i64)]);
                    visited += 1;
                    true
                })
                .unwrap(),
            Some(ids.len())
        );
        assert_eq!(visited, ids.len());
        let actual = selects.load(Ordering::Relaxed);
        store
            .conn
            .with_physical(|sqlite| {
                sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
                sqlite.set_prepared_statement_cache_capacity(
                    crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
                );
                Ok(())
            })
            .unwrap();
        assert!(
            actual <= 24,
            "{count} consecutive rows prepared {actual} SELECTs"
        );
    }
}

#[test]
fn consecutive_points_keep_missing_private_and_historical_blob_rows() {
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    for id in 1..=16 {
        store.put(id, document(id as i64)).unwrap();
    }
    store.conn.commit_transaction().unwrap();
    let old = store.snapshot().unwrap();
    store.conn.begin_transaction().unwrap();
    store.delete(4).unwrap();
    store.put(8, document(80)).unwrap();
    store.put(17, document(170)).unwrap();
    let private = store.snapshot().unwrap();
    assert_captured_points(private.as_ref(), 1);
    store.conn.rollback_transaction().unwrap();
    store.put(8, document(800)).unwrap();
    let current = store.snapshot().unwrap();
    for (view, kind) in [(old, 0), (private, 1), (current, 2)] {
        assert_captured_points(view.as_ref(), kind);
    }
}

fn assert_captured_points(view: &dyn DocumentStore, kind: u8) {
    let ids = (0..=18).collect::<Vec<_>>();
    let mut visited = 0;
    assert_eq!(
        view.for_each_fields_multi_borrowed(
            &ids,
            &["value", "absent", "vector", "value"],
            &mut |id, present, fields| {
                assert_eq!(id, ids[visited]);
                visited += 1;
                let expected = match (kind, id) {
                    (1, 4) => None,
                    (1, 8) => Some(80),
                    (1, 17) => Some(170),
                    (2, 8) => Some(800),
                    (_, 1..=16) => Some(id as i64),
                    _ => None,
                };
                assert_eq!(present, expected.is_some());
                assert_eq!(fields[0], &expected.map_or(Value::Null, Value::Int));
                assert_eq!(fields[1], &Value::Null);
                assert_eq!(
                    fields[2],
                    &expected.map_or(Value::Null, |value| document(value)
                        .remove("vector")
                        .unwrap())
                );
                assert!(std::ptr::eq(fields[0], fields[3]));
                true
            }
        )
        .unwrap(),
        Some(ids.len())
    );
}

#[test]
fn consecutive_points_admit_only_requested_bodies_before_the_visitor_stops() {
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    store
        .put(
            1,
            BTreeMap::from([("body".into(), Value::Str("small".into()))]),
        )
        .unwrap();
    store
        .put(
            9,
            BTreeMap::from([("body".into(), Value::Str("large".repeat(1 << 18)))]),
        )
        .unwrap();
    store.conn.commit_transaction().unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - 65536)
        .unwrap();
    let baseline = control.memory().used();
    // A missing tail must not advance to and admit the first row beyond the requested bound.
    let ids = (1..=8).collect::<Vec<_>>();
    assert_eq!(
        snapshot
            .for_each_fields_multi_borrowed(&ids, &["body"], &mut |id, present, values| {
                assert_eq!(present, id == 1);
                assert_eq!(
                    values,
                    [&if present {
                        Value::Str("small".into())
                    } else {
                        Value::Null
                    }]
                );
                true
            })
            .unwrap(),
        Some(8)
    );
    // Stop on an absent identity before admitting the next stored body in the range.
    let ids = (2..=9).collect::<Vec<_>>();
    assert_eq!(
        snapshot
            .for_each_fields_multi_borrowed(&ids, &["body"], &mut |id, present, _| {
                assert_eq!(id, 2);
                assert!(!present);
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(control.memory().used(), baseline);
    let mut visited = 0;
    assert!(matches!(
        snapshot.for_each_fields_multi_borrowed(&ids, &["body"], &mut |_, _, _| {
            visited += 1;
            true
        }),
        Err(uqa_storage::StorageBackendError::Memory(_))
    ));
    assert_eq!(visited, 7);
    assert_eq!(control.memory().used(), baseline);
    assert!(matches!(
        snapshot.for_each_fields_multi_borrowed(&ids, &["body"], &mut |_, _, _| {
            control.cancellation().cancel();
            false
        }),
        Err(uqa_storage::StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    drop(hold);
}

#[test]
fn consecutive_points_recheck_latest_commit_after_blob_callback_reentry() {
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    for id in 1..=8 {
        store
            .put(
                id,
                if id == 2 {
                    document(2)
                } else {
                    BTreeMap::from([("value".into(), Value::Int(id as i64))])
                },
            )
            .unwrap();
    }
    store.conn.commit_transaction().unwrap();
    let snapshot = store.snapshot().unwrap();
    let ids = (1..=8).collect::<Vec<_>>();
    assert_eq!(
        snapshot
            .for_each_fields_multi_borrowed(
                &ids,
                &["value", "vector"],
                &mut |id, present, fields| {
                    assert!(present);
                    assert_eq!(fields[0], &Value::Int(id as i64));
                    if id == 2 {
                        store.put(3, document(30)).unwrap();
                    }
                    true
                }
            )
            .unwrap(),
        Some(8)
    );
    assert_eq!(store.get(3).unwrap().unwrap()["value"], Value::Int(30));
}
