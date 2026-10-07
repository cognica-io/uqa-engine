//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Historical scans pay version-lookup work for changed rows instead of every row.

use super::*;
use crate::mvcc::native::{NativeRecordFamily, NativeRecordIdentity};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn historical_projection_avoids_per_row_version_resolution() {
    for count in [64_u64, 256] {
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
        let retained = store.snapshot().unwrap();
        let native = store.conn.native_snapshot().unwrap().unwrap();
        let owner = native.table_owner("docs").unwrap().unwrap();
        let identity = NativeRecordIdentity::new(NativeRecordFamily::Documents, owner).unwrap();
        let prefix = identity.encode_prefix(&[], &native.control).unwrap();
        store.conn.begin_transaction().unwrap();
        store
            .put(2, BTreeMap::from([("value".into(), Value::Int(-2))]))
            .unwrap();
        store.delete(4).unwrap();
        store
            .put(
                count + 1,
                BTreeMap::from([("value".into(), Value::Int(-1))]),
            )
            .unwrap();
        store.conn.commit_transaction().unwrap();
        let steps = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&steps);
        store
            .conn
            .with_physical(|sqlite| {
                sqlite.progress_handler(
                    1,
                    Some(move || {
                        counter.fetch_add(1, Ordering::Relaxed);
                        false
                    }),
                )?;
                Ok(())
            })
            .unwrap();
        let mut records = 0;
        native
            .view
            .visit_prefix(
                &prefix,
                None,
                usize::MAX,
                &native.control,
                &mut |_, record| {
                    records += u64::from(record.value.is_some());
                    Ok(true)
                },
            )
            .unwrap();
        assert_eq!(records, count);
        let reference = steps.swap(0, Ordering::Relaxed);
        let actual = scan(retained.as_ref(), None, usize::MAX, &["value"]);
        let projected = steps.load(Ordering::Relaxed);
        eprintln!("{count} historical rows: projected={projected}, record scan={reference}");
        store
            .conn
            .with_physical(|sqlite| {
                sqlite.progress_handler(0, None::<fn() -> bool>)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            actual,
            (1..=count)
                .map(|id| (id, vec![Value::Int(id as i64)]))
                .collect::<Vec<_>>()
        );
        assert!(
            projected * 4 < reference * 3,
            "{count} rows: projected={projected}, record scan={reference}"
        );
    }
}

#[test]
fn historical_projection_merges_captured_private_rows_after_rollback() {
    let mut store = fixture();
    let document = |value| {
        BTreeMap::from([
            ("value".into(), Value::Int(value)),
            ("raw".into(), Value::Bytes(vec![value as u8; 8192])),
        ])
    };
    for id in [10, 20, 30, 40] {
        store.put(id, document(id as i64)).unwrap();
    }
    store.conn.begin_transaction().unwrap();
    for (id, value) in [(5, 5), (20, 200), (50, 50)] {
        store.put(id, document(value)).unwrap();
    }
    store.delete(30).unwrap();
    let captured = store.snapshot().unwrap();
    store.conn.rollback_transaction().unwrap();
    store.conn.begin_transaction().unwrap();
    for id in [10, 15, 20] {
        store.put(id, document(-1)).unwrap();
    }
    store.delete(40).unwrap();
    store.conn.commit_transaction().unwrap();
    let expected: Vec<_> = [(5, 5), (10, 10), (20, 200), (40, 40), (50, 50)]
        .into_iter()
        .map(|(id, value)| {
            (
                id,
                vec![
                    Value::Int(value),
                    Value::Bytes(vec![value as u8; 8192]),
                    Value::Null,
                ],
            )
        })
        .collect();
    for (after, limit) in [
        (None, usize::MAX),
        (None, 1),
        (Some(5), 2),
        (Some(20), 4),
        (Some(50), 1),
    ] {
        let actual = scan(captured.as_ref(), after, limit, &["value", "raw", "absent"]);
        assert_eq!(
            actual,
            expected
                .iter()
                .filter(|(id, _)| after.is_none_or(|after| *id > after))
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn historical_projection_stops_before_admitting_later_values() {
    let mut store = fixture();
    for (id, body) in [(1, "small".into()), (2, "large".repeat(1 << 18))] {
        store
            .put(id, BTreeMap::from([("body".into(), Value::Str(body))]))
            .unwrap();
    }
    let captured = store.snapshot().unwrap();
    for id in [1, 2, 3] {
        store
            .put(
                id,
                BTreeMap::from([("body".into(), Value::Str("new".into()))]),
            )
            .unwrap();
    }
    let control = store.conn.retention_control().unwrap();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - 65536)
        .unwrap();
    let baseline = control.memory().used();
    assert_eq!(
        captured
            .for_each_next_fields_borrowed(None, 3, &["body"], &mut |id, values| {
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
        captured.for_each_next_fields_borrowed(None, 3, &["body"], &mut |_, _| {
            visited += 1;
            true
        }),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(visited, 1);
    assert_eq!(control.memory().used(), baseline);
    drop(hold);
    control.cancellation().cancel();
    assert!(matches!(
        captured.for_each_next_fields_borrowed(None, 1, &["body"], &mut |_, _| false),
        Err(StorageBackendError::Cancelled(_))
    ));
}
