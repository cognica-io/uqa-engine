//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

use super::{expect, expect_eq, key};

/// Verify pre-materialization value caps, retained private visibility, tombstones and current cancellation on a disposable versioned store.
pub fn verify_bounded_value_reads(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
    let key = key(b"bounded-value");
    let large = [7; 4096];
    let private = [9; 64];
    store.put(&key, &large)?;
    let query = StorageReadControl::with_limit(16_384);
    rejected(store, &key, large.len() - 1, &query)?;
    expect(
        query.memory().peak() < large.len(),
        "oversize value rejected before charging its payload",
    )?;
    selected(store, &key, large.len(), Some(&large), &query)?;

    store.begin_transaction()?;
    store.put(&key, &private)?;
    rejected(store, &key, private.len() - 1, &query)?;
    selected(store, &key, private.len(), Some(&private), &query)?;
    let retained = store.open_retained_read_session(query.cancellation())?;
    store.rollback_transaction()?;
    selected(&*retained, &key, private.len(), Some(&private), &query)?;
    rejected(store, &key, private.len(), &query)?;

    store.delete(&key)?;
    selected(store, &key, 0, None, &query)?;
    selected(store, &super::key(b"bounded-missing"), 0, None, &query)?;
    query.cancellation().cancel();
    let error = retained.visit_value_bounded(&key, private.len(), &query, &mut |_| {
        Err(StorageBackendError::Other(
            "cancelled bounded read reached its visitor".into(),
        ))
    });
    expect(
        matches!(error, Err(StorageBackendError::Cancelled(_))),
        "bounded read preserves cancellation",
    )?;
    let next = StorageReadControl::with_limit(16_384);
    selected(&*retained, &key, private.len(), Some(&private), &next)?;
    expect_eq(&query.memory().used(), &0, "bounded read query release")?;
    expect_eq(&next.memory().used(), &0, "independent read query release")
}

fn rejected(
    store: &dyn KeyValueStore,
    key: &[u8],
    maximum: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut visited = false;
    let result = store.visit_value_bounded(key, maximum, control, &mut |_| {
        visited = true;
        Ok(())
    });
    expect(
        matches!(result, Err(StorageBackendError::Memory(_))) && !visited,
        "bounded value rejection precedes its visitor",
    )?;
    expect_eq(&control.memory().used(), &0, "failed bounded read release")
}

fn selected(
    store: &dyn KeyValueStore,
    key: &[u8],
    maximum: usize,
    expected: Option<&[u8]>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut count = 0;
    store.visit_value_bounded(key, maximum, control, &mut |value| {
        count += 1;
        expect_eq(&value, &expected, "bounded value snapshot bytes")
    })?;
    expect_eq(&count, &1, "bounded value completion count")?;
    expect_eq(&control.memory().used(), &0, "bounded value release")
}
