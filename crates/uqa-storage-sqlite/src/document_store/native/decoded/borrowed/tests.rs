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
            sqlite.set_prepared_statement_cache_capacity(
                crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
            );
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

type Scanned = Vec<(u64, Vec<Value>)>;

/// Scan `fields` and record which physical tables the scan's statements read.
fn scan_reading(
    store: &SQLiteDocumentStore,
    snapshot: &dyn DocumentStore,
    after: Option<u64>,
    limit: usize,
    fields: &[&str],
) -> (Scanned, std::collections::BTreeSet<String>) {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{Arc, Mutex};
    let tables = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let seen = Arc::clone(&tables);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = context.action {
                    seen.lock().unwrap().insert(table_name.to_owned());
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut scanned = Vec::new();
    snapshot
        .for_each_next_fields_borrowed(after, limit, fields, &mut |id, values| {
            scanned.push((id, values.iter().map(|value| (*value).clone()).collect()));
            true
        })
        .unwrap();
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(
                crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
            );
            Ok(())
        })
        .unwrap();
    let tables = tables.lock().unwrap().clone();
    (scanned, tables)
}

#[test]
fn latest_committed_rows_read_the_physical_projection_and_match_the_records() {
    let mut store = fixture();
    let mut other = SQLiteDocumentStore::new(store.conn.clone(), "other");
    for id in 1..=300_u64 {
        let mut fields = BTreeMap::from([("value".into(), Value::Int(id as i64))]);
        if id == 150 {
            // A body above the inline limit is admitted and read by itself.
            fields.extend((0..2000).map(|field| (format!("f{field}"), Value::Int(field))));
        }
        if id % 50 == 0 {
            // Values stored outside the body end a physical read to hydrate.
            fields.insert(
                "body".into(),
                Value::Bytes(format!("body{id}").repeat(2048).into_bytes()),
            );
        }
        store.put(id, fields).unwrap();
    }
    let stale = store.snapshot().unwrap();
    other
        .put(1, BTreeMap::from([("x".into(), Value::Int(1))]))
        .unwrap();
    let latest = store.snapshot().unwrap();
    for (after, limit) in [
        (None, usize::MAX),
        (Some(100), 120),
        (Some(149), 3),
        (None, 1),
    ] {
        for fields in [&["value"][..], &["body", "value", "f7"][..]] {
            let (expected, record_tables) =
                scan_reading(&store, stale.as_ref(), after, limit, fields);
            let (actual, latest_tables) =
                scan_reading(&store, latest.as_ref(), after, limit, fields);
            assert_eq!(
                actual, expected,
                "after {after:?}, limit {limit}, {fields:?}"
            );
            assert!(!record_tables.contains("_documents"), "{record_tables:?}");
            // The first complete scan of ["value"] caches its column; byte values are stored outside the body and never cached.
            let cached = fields == ["value"] && (after, limit) != (None, usize::MAX);
            assert_eq!(
                latest_tables.contains("_documents"),
                !cached,
                "after {after:?}, limit {limit}, {fields:?}: {latest_tables:?}"
            );
        }
    }
}

#[test]
fn private_records_keep_the_record_scan() {
    let mut store = fixture();
    for id in 1..=3 {
        store
            .put(
                id,
                BTreeMap::from([("value".into(), Value::Int(id as i64))]),
            )
            .unwrap();
    }
    store.conn.begin_transaction().unwrap();
    store
        .put(2, BTreeMap::from([("value".into(), Value::Int(20))]))
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    // The committed projection still holds 2; only the private records hold 20.
    let mut scanned = Vec::new();
    snapshot
        .for_each_next_fields_borrowed(None, usize::MAX, &["value"], &mut |id, values| {
            scanned.push((id, values[0].clone()));
            true
        })
        .unwrap();
    assert_eq!(
        scanned,
        [(1, Value::Int(1)), (2, Value::Int(20)), (3, Value::Int(3))]
    );
    store.conn.rollback_transaction().unwrap();
}

#[test]
fn repeated_latest_scans_serve_decoded_columns_until_the_table_changes() {
    let mut store = fixture();
    let mut other = SQLiteDocumentStore::new(store.conn.clone(), "other");
    for id in 1..=200_u64 {
        store
            .put(
                id,
                BTreeMap::from([
                    ("value".into(), Value::Int(id as i64)),
                    ("label".into(), Value::Str(format!("item {id}"))),
                    ("raw".into(), Value::Bytes(vec![1, 2, 3])),
                ]),
            )
            .unwrap();
    }
    let fields = ["value", "label"];
    let first = store.snapshot().unwrap();
    let (expected, tables) = scan_reading(&store, first.as_ref(), None, usize::MAX, &fields);
    assert_eq!(expected.len(), 200);
    assert!(tables.contains("_documents"), "{tables:?}");
    let (cached, tables) = scan_reading(&store, first.as_ref(), None, usize::MAX, &fields);
    assert_eq!(cached, expected);
    assert!(!tables.contains("_documents"), "{tables:?}");
    let (page, tables) = scan_reading(&store, first.as_ref(), Some(100), 20, &fields);
    assert_eq!(page, expected[100..120]);
    assert!(!tables.contains("_documents"), "{tables:?}");
    // Another table's commit leaves this table's generation, and its columns, unchanged.
    other
        .put(1, BTreeMap::from([("x".into(), Value::Int(1))]))
        .unwrap();
    let second = store.snapshot().unwrap();
    let (unchanged, tables) = scan_reading(&store, second.as_ref(), None, usize::MAX, &fields);
    assert_eq!(unchanged, expected);
    assert!(!tables.contains("_documents"), "{tables:?}");
    // A commit to the table advances its generation, so the next scan reads and decodes rows again.
    store
        .put(5, BTreeMap::from([("value".into(), Value::Int(500))]))
        .unwrap();
    let third = store.snapshot().unwrap();
    let (changed, tables) = scan_reading(&store, third.as_ref(), None, usize::MAX, &fields);
    assert!(tables.contains("_documents"), "{tables:?}");
    assert_eq!(changed[4], (5, vec![Value::Int(500), Value::Null]));
    // Non-scalar columns are never cached.
    for _ in 0..2 {
        let (raw, tables) = scan_reading(&store, third.as_ref(), None, usize::MAX, &["raw"]);
        assert_eq!(raw.len(), 200);
        assert!(tables.contains("_documents"), "{tables:?}");
    }
}

#[test]
fn paged_latest_scans_build_decoded_columns_across_pages() {
    let mut store = fixture();
    for id in 1..=200_u64 {
        store
            .put(
                id,
                BTreeMap::from([("value".into(), Value::Int(id as i64))]),
            )
            .unwrap();
    }
    let snapshot = store.snapshot().unwrap();
    let page = |after: Option<u64>| scan_reading(&store, snapshot.as_ref(), after, 64, &["value"]);
    // 200 rows in pages of 64: the fourth page reaches the last row and records the columns.
    let mut first = Vec::new();
    let mut after = None;
    loop {
        let (rows, tables) = page(after);
        if rows.is_empty() {
            // The page that reached the last row recorded the columns, so this page reads them.
            assert!(!tables.contains("_documents"), "{tables:?}");
            break;
        }
        assert!(tables.contains("_documents"), "{tables:?}");
        after = rows.last().map(|(id, _)| *id);
        first.extend(rows);
    }
    assert_eq!(first.len(), 200);
    let mut second = Vec::new();
    let mut after = None;
    loop {
        let (rows, tables) = page(after);
        assert!(!tables.contains("_documents"), "{tables:?}");
        if rows.is_empty() {
            break;
        }
        after = rows.last().map(|(id, _)| *id);
        second.extend(rows);
    }
    assert_eq!(second, first);
}
