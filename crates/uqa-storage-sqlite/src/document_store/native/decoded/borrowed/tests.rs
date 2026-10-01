//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use std::collections::BTreeMap;
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
fn borrowed_native_rows_use_bounded_queries_and_keep_the_decoding_allowance() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    for id in 1..=1024 {
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
    let queries = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&queries);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(context.action, AuthAction::Select) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut ids = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields_borrowed(
                None,
                1024,
                &["value", "missing", "value"],
                &mut |id, values| {
                    assert_eq!(values[0], &Value::Int(id as i64));
                    assert_eq!(values[1], &Value::Null);
                    assert!(std::ptr::eq(values[0], values[2]));
                    assert!(control.memory().used() > retained);
                    ids.push(id);
                    true
                }
            )
            .unwrap(),
        Some(1024)
    );
    assert_eq!(ids, (1..=1024).collect::<Vec<_>>());
    assert!(
        queries.load(Ordering::Relaxed) <= 30,
        "projected scan performed {} SELECTs",
        queries.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), retained);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
}

#[test]
fn borrowed_native_rows_hydrate_selected_blobs_on_the_original_boundary() {
    let mut store = fixture();
    for id in [1, 3, 5] {
        store
            .put(
                id,
                BTreeMap::from([
                    ("value".into(), Value::Int(id as i64)),
                    ("body".into(), Value::Str(format!("body{id}").repeat(2048))),
                ]),
            )
            .unwrap();
    }
    let snapshot = store.snapshot().unwrap();
    store
        .put(
            3,
            BTreeMap::from([("body".into(), Value::Str("replacement".into()))]),
        )
        .unwrap();
    let mut actual = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields_borrowed(Some(1), 2, &["body", "value"], &mut |id, values| {
                assert_eq!(values[0], &Value::Str(format!("body{id}").repeat(2048)));
                actual.push(id);
                true
            })
            .unwrap(),
        Some(2)
    );
    assert_eq!(actual, [3, 5]);
    let mut actual = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields_borrowed(None, 3, &["value"], &mut |id, _| {
                actual.push(id);
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(actual, [1]);
}

#[test]
fn borrowed_native_rows_stop_before_later_payload_admission_and_release_failed_reads() {
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
    assert_eq!(
        snapshot
            .for_each_next_fields_borrowed(None, 2, &["body"], &mut |id, values| {
                assert_eq!(id, 1);
                assert_eq!(values, [&Value::Str("small".into())]);
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(control.memory().used(), baseline);
    let mut visited = 0;
    assert!(matches!(
        snapshot.for_each_next_fields_borrowed(None, 2, &["body"], &mut |_, _| {
            visited += 1;
            true
        }),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(visited, 1);
    assert_eq!(control.memory().used(), baseline);
    drop(hold);
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn borrowed_native_rows_honor_cancellation_after_the_last_visitor() {
    let mut store = fixture();
    store
        .put(1, BTreeMap::from([("value".into(), Value::Int(1))]))
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let retained = control.memory().used();
    let mut visited = 0;
    assert!(matches!(
        snapshot.for_each_next_fields_borrowed(None, 1, &["value"], &mut |_, _| {
            visited += 1;
            control.cancellation().cancel();
            false
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(visited, 1);
    assert_eq!(control.memory().used(), retained);
    assert!(matches!(
        snapshot
            .for_each_next_fields_borrowed(None, 0, &["value"], &mut |_, _| panic!("empty cursor")),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
}

#[test]
fn native_projection_avoids_unselected_inline_payload_allocation_and_keeps_field_presence() {
    let mut store = fixture();
    for id in 1..=8 {
        store
            .put(
                id,
                BTreeMap::from([
                    ("body".into(), Value::Str("x".repeat(4096))),
                    ("value".into(), Value::Int(id as i64)),
                ]),
            )
            .unwrap();
    }
    let snapshot = store.snapshot().unwrap();
    let visit = |fields: &[&str]| {
        assert_eq!(
            snapshot
                .for_each_next_fields_borrowed(None, 8, fields, &mut |id, values| {
                    assert_eq!(values[0], &Value::Int(id as i64));
                    true
                })
                .unwrap(),
            Some(8)
        );
    };
    visit(&["value"]);
    visit(&["value", "body"]);
    let selected = allocation_counter::measure(|| visit(&["value"]));
    let complete = allocation_counter::measure(|| visit(&["value", "body"]));
    assert!(selected.bytes_total + 8 * 4096 <= complete.bytes_total);
    let control = uqa_storage::read_control::StorageReadControl::with_limit(1 << 20);
    let presence = snapshot
        .field_presence_controlled(&[1, 99], &["body", "value", "missing"], &control)
        .unwrap();
    assert_eq!(&*presence, &[true, true, false, false, false, false]);
    drop(presence);
    assert_eq!(control.memory().used(), 0);
}
