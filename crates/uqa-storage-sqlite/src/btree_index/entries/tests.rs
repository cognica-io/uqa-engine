//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentStore};

fn fixture(
    native: bool,
) -> (
    ManagedConnection,
    SQLiteBTreeIndexStore,
    Vec<(DocId, Value)>,
) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection.begin_transaction().unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    let values: Vec<_> = (1..=2048)
        .map(|id| (id, Value::Str(format!("{id}:{}", "한".repeat(1366)))))
        .collect();
    for (id, value) in &values {
        documents
            .put(*id, [("key".into(), value.clone())].into())
            .unwrap();
    }
    let index = SQLiteBTreeIndexStore::new(connection.clone());
    index.replace("items", &"key".into(), &values).unwrap();
    connection.commit_transaction().unwrap();
    if native {
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
    }
    (connection, index, values)
}

#[test]
fn stored_entries_stream_wide_values_on_native_and_legacy_boundaries() {
    for native in [false, true] {
        let (_, index, values) = fixture(native);
        let mut count = 0;
        let allocation = allocation_counter::measure(|| {
            assert!(index
                .visit_entries("items", &"key".into(), &mut |id, value| {
                    assert_eq!((id, &value), (values[count].0, &values[count].1));
                    count += 1;
                    Ok(())
                })
                .unwrap());
        });
        assert_eq!(count, values.len());
        assert!(
            allocation.bytes_max < 512 * 1024,
            "{native}: {allocation:?}"
        );
        assert!(!index
            .visit_entries("items", &"absent".into(), &mut |_, _| panic!("unbuilt"))
            .unwrap());
        assert_eq!(
            index.load("items", &"key".into()).unwrap().as_ref(),
            Some(&values)
        );
    }
}

#[test]
fn stored_entry_visitors_preserve_private_values_rollback_and_typed_failure() {
    for native in [false, true] {
        let (connection, index, _) = fixture(native);
        connection.begin_transaction().unwrap();
        index
            .replace(
                "items",
                &"key".into(),
                &[(2, Value::Null), (3, Value::Bytes(vec![0, 255]))],
            )
            .unwrap();
        let mut values = Vec::new();
        assert!(index
            .visit_entries("items", &"key".into(), &mut |id, value| {
                values.push((id, value));
                Ok(())
            })
            .unwrap());
        assert_eq!(values, [(2, Value::Null), (3, Value::Bytes(vec![0, 255]))]);
        let mut count = 0;
        let error = index
            .visit_entries("items", &"key".into(), &mut |_, _| {
                count += 1;
                Err(uqa_core::memory::MemoryError::Limit {
                    required: 7,
                    limit: 3,
                }
                .into())
            })
            .unwrap_err();
        assert!(matches!(
            error,
            uqa_storage::StorageBackendError::Memory(uqa_core::memory::MemoryError::Limit {
                required: 7,
                limit: 3
            })
        ));
        assert_eq!(count, 1);
        connection.rollback_transaction().unwrap();
        let mut count = 0;
        index
            .visit_entries("items", &"key".into(), &mut |id, value| {
                count += 1;
                assert_eq!(id, count);
                assert!(matches!(value, Value::Str(_)));
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 2048);
    }
}

#[test]
fn stored_entry_visitors_keep_historical_values_and_cancel_before_delivery() {
    let (connection, index, original) = fixture(true);
    let older = connection.new_session();
    older.begin_deferred_transaction().unwrap();
    let old_index = SQLiteBTreeIndexStore::new(older.clone());
    let capture = |source: &SQLiteBTreeIndexStore| {
        let mut values = Vec::new();
        source
            .visit_entries("items", &"key".into(), &mut |id, value| {
                values.push((id, value));
                Ok(())
            })
            .unwrap();
        values
    };
    assert_eq!(capture(&old_index), original);
    connection.begin_transaction().unwrap();
    index
        .replace("items", &"key".into(), &[(2, Value::Null)])
        .unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(capture(&index), [(2, Value::Null)]);
    assert_eq!(capture(&old_index), original);
    let snapshot = older.native_snapshot().unwrap().unwrap();
    snapshot.control.cancellation().cancel();
    let result = super::native::visit_entries(&snapshot, "items", &"key".into(), &mut |_, _| {
        panic!("cancelled snapshot delivered a value")
    });
    assert!(matches!(result, Err(crate::SQLiteError::Cancelled(_))));
    snapshot.control.cancellation().reset();
    older.rollback_transaction().unwrap();
    assert_eq!(capture(&old_index), [(2, Value::Null)]);
}
