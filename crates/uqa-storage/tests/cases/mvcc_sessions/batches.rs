//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated batches transfer their retained bytes into private transaction history.

use super::*;

#[test]
fn evaluated_batch_payloads_fit_one_retained_copy_through_savepoint_undo() {
    let persistence = Persistence::new();
    let store = persistence.session(96 * 1024);
    let control = store.retention_control();
    let payload = vec![7; 64 * 1024];
    store.begin_transaction().unwrap();
    store.savepoint("before").unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put(b"payload", &payload))
        .unwrap();
    let retained = store.record_snapshot().unwrap();
    store.rollback_to_savepoint("before").unwrap();
    assert_eq!(store.get(b"payload").unwrap(), None);
    retained
        .visit_value(b"payload", &control, &mut |record| {
            assert_eq!(record.unwrap().value, Some(payload.as_slice()));
            Ok(())
        })
        .unwrap();
    store.rollback_transaction().unwrap();
    assert!(control.memory().used() >= payload.len());
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}
