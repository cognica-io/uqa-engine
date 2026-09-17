//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared conformance schedules for definition reads and immutable data-revision markers.

use crate::{KeyValueStore, StorageBackendResult};

fn data(store: &dyn KeyValueStore, row: &[u8]) -> StorageBackendResult<()> {
    store.with_mutation(&mut |read, batch| {
        assert!(read.get(b"guard-definition")?.is_some());
        batch.require_unchanged(b"guard-definition")?;
        batch.touch_marker(b"guard-data", b"data-v1")?;
        batch.put(row, b"data")
    })
}

fn definition(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
    let mut batch = store.batch();
    batch.fence_record(b"guard-data")?;
    batch.put(b"guard-definition", b"replacement")?;
    batch.commit()
}

/// Exercise two independent sessions over the same disposable database. Independent data writers must both publish, while a definition change and a data writer cannot publish incompatible views in either commit order.
pub fn verify_revision_guards(
    a: &dyn KeyValueStore,
    b: &dyn KeyValueStore,
) -> StorageBackendResult<()> {
    a.put(b"guard-definition", b"initial")?;
    a.begin_transaction()?;
    b.begin_transaction()?;
    data(a, b"guard-row-a")?;
    data(b, b"guard-row-b")?;
    b.commit_transaction()?;
    assert!(a.in_transaction());
    assert!(a.get(b"guard-row-b")?.is_none());
    a.commit_transaction()?;
    assert_eq!(a.get(b"guard-row-a")?.as_deref(), Some(&b"data"[..]));
    assert_eq!(a.get(b"guard-row-b")?.as_deref(), Some(&b"data"[..]));
    a.begin_transaction()?;
    b.begin_transaction()?;
    for store in [a, b] {
        let mut check = store.batch();
        check.require_unchanged(b"guard-definition")?;
        check.commit()?;
    }
    a.commit_transaction()?;
    b.commit_transaction()?;
    for data_wins in [false, true] {
        a.begin_transaction()?;
        b.begin_transaction()?;
        data(a, b"guard-racing-row")?;
        definition(b)?;
        let (winner, loser) = if data_wins { (a, b) } else { (b, a) };
        winner.commit_transaction()?;
        assert!(loser.commit_transaction().is_err());
        loser.rollback_transaction()?;
        assert_eq!(a.get(b"guard-racing-row")?.is_some(), data_wins);
    }
    a.begin_transaction()?;
    let mut check = a.batch();
    check.require_unchanged(b"guard-definition")?;
    check.commit()?;
    b.put(b"guard-definition", b"changed-again")?;
    assert!(a.commit_transaction().is_err());
    a.rollback_transaction()?;
    a.begin_transaction()?;
    a.savepoint("before-dependency")?;
    data(a, b"guard-undone-row")?;
    a.rollback_to_savepoint("before-dependency")?;
    b.put(b"guard-definition", b"after-undo")?;
    a.put(b"guard-unrelated", b"kept")?;
    a.commit_transaction()?;
    assert!(a.get(b"guard-undone-row")?.is_none());
    assert_eq!(a.get(b"guard-data")?.as_deref(), Some(&b"data-v1"[..]));
    Ok(())
}
