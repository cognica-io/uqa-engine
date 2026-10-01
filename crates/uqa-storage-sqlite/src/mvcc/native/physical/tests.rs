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
