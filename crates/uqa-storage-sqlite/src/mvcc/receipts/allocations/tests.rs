//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn an_occupied_reserve_allowance_cannot_block_an_independent_owner() {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        crate::ManagedConnection::open(&directory.path().join("allowance.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let pool = ManagedAllocations::default();
    let occupied = pool.0.memory.reserve(RETAINED_BYTES).unwrap();
    let owner = pool.allocate(&store, &control).unwrap();
    assert_eq!(owner.transaction().allocation(), 1);
    assert_eq!(
        store
            .read(|connection| Ok(codec::header(connection, store.identity)?.allocated))
            .unwrap(),
        1
    );
    assert_eq!(pool.0.memory.used(), RETAINED_BYTES);
    drop(owner);
    assert_eq!(control.memory().used(), 0);
    drop(occupied);
    let owner = pool.allocate(&store, &control).unwrap();
    assert_eq!(owner.transaction().allocation(), 2);
    assert_eq!(
        store
            .read(|connection| Ok(codec::header(connection, store.identity)?.allocated))
            .unwrap(),
        2
    );
}
