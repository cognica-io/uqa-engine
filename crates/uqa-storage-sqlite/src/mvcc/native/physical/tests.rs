//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical statement reuse preserves fresh parameters and transactional results.

use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn physical_row_operations_reuse_preparation_with_fresh_bindings() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE _metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    let compiled = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&compiled);
    connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Select | AuthAction::Insert { .. } | AuthAction::Delete { .. }
            ) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            Authorization::Allow
        }))
        .unwrap();
    let control = StorageReadControl::with_limit(65536);
    let layout = super::super::NativeRecordFamily::Metadata.layout();
    let transaction = connection.unchecked_transaction().unwrap();
    for number in 0..128 {
        let key = format!("key:{number:04}\0%_");
        let text = format!("value:{number}");
        let values = [
            ValueRef::Text(key.as_bytes()),
            ValueRef::Text(text.as_bytes()),
        ];
        upsert(&transaction, layout, &values, &control).unwrap();
        let encoded_key = physical_key(layout, &values, &control).unwrap();
        let found = get(&transaction, layout, &encoded_key, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*decode_row(&found, 2, &control).unwrap(), &values);
        remove(&transaction, layout, &encoded_key, &control).unwrap();
        assert!(get(&transaction, layout, &encoded_key, &control)
            .unwrap()
            .is_none());
        upsert(&transaction, layout, &values, &control).unwrap();
    }
    assert!(
        compiled.load(Ordering::Relaxed) <= 8,
        "fixed row operations must not compile statements per row"
    );
    transaction.rollback().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM _metadata", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(control.memory().used(), 0);
    connection
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
}

#[test]
fn a_row_is_read_with_its_size_until_it_exceeds_the_inline_limit() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE _metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    let layout = super::super::NativeRecordFamily::Metadata.layout();
    let control = StorageReadControl::with_limit(4 << 20);
    // The key and the value of the second row total exactly the inline limit; the third is one byte over it.
    let rows = [
        ("a", String::new()),
        ("b", "v".repeat(INLINE_ROW_BYTES - 1)),
        ("c", "v".repeat(INLINE_ROW_BYTES)),
        ("d", "v".repeat(300_000)),
    ];
    let mut keys = Vec::new();
    for (key, value) in &rows {
        let values = [
            ValueRef::Text(key.as_bytes()),
            ValueRef::Text(value.as_bytes()),
        ];
        upsert(&connection, layout, &values, &control).unwrap();
        keys.push(physical_key(layout, &values, &control).unwrap());
    }
    for ((key, value), encoded) in rows.iter().zip(&keys) {
        let found = get(&connection, layout, encoded, &control)
            .unwrap()
            .unwrap();
        assert_eq!(
            &*decode_row(&found, 2, &control).unwrap(),
            &[
                ValueRef::Text(key.as_bytes()),
                ValueRef::Text(value.as_bytes())
            ]
        );
    }
    let mut visited = Vec::new();
    visit(&connection, layout, &control, |values| {
        visited.push(values[1].as_str().unwrap().len());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        visited,
        [0, INLINE_ROW_BYTES - 1, INLINE_ROW_BYTES, 300_000]
    );
    drop(keys);
    assert_eq!(control.memory().used(), 0);

    // A row over the inline limit is read only after three times its size is reserved; the rows under it still fit.
    let narrow = StorageReadControl::with_limit(64 * 1024);
    let encoded = |key: &str| {
        physical_key(
            layout,
            &[ValueRef::Text(key.as_bytes()), ValueRef::Text(b"")],
            &narrow,
        )
        .unwrap()
    };
    assert!(get(&connection, layout, &encoded("c"), &narrow)
        .unwrap()
        .is_some());
    assert!(matches!(
        get(&connection, layout, &encoded("d"), &narrow),
        Err(crate::mvcc::Error::Version(VersionError::Memory(_)))
    ));
    assert!(get(&connection, layout, &encoded("missing"), &narrow)
        .unwrap()
        .is_none());
    assert_eq!(narrow.memory().used(), 0);
}

#[test]
fn binary_rows_reserve_binary_bytes_and_preserve_text_conversion() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!(
                "PRAGMA encoding = '{encoding}'; CREATE TABLE _schemas (name TEXT PRIMARY KEY, role_owner TEXT NOT NULL, acl_json TEXT)"
            ))
            .unwrap();
        let layout = super::super::NativeRecordFamily::Schemas.layout();
        let control = StorageReadControl::with_limit(40 * 1024);
        let binary = vec![0xa7; 16 * 1024];
        let text = "가\0é🌲".repeat(128);
        for owner in [ValueRef::Blob(&binary), ValueRef::Text(text.as_bytes())] {
            let values = [
                ValueRef::Text(b"namespace"),
                owner,
                ValueRef::Text(text.as_bytes()),
            ];
            upsert(&connection, layout, &values, &control).unwrap();
            let key = physical_key(layout, &values, &control).unwrap();
            let row = get(&connection, layout, &key, &control).unwrap().unwrap();
            assert_eq!(&*decode_row(&row, 3, &control).unwrap(), &values);
            drop(row);
            let mut count = 0;
            visit(&connection, layout, &control, |found| {
                assert_eq!(found, values);
                count += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(count, 1);
            let narrow = StorageReadControl::with_limit(1024);
            assert!(matches!(
                get(&connection, layout, &key, &narrow),
                Err(crate::mvcc::Error::Version(VersionError::Memory(_)))
            ));
            assert_eq!(narrow.memory().used(), 0);
        }
        assert_eq!(control.memory().used(), 0);
    }
}
