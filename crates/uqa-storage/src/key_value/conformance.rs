//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reusable conformance checks for third-party [`super::KeyValueStore`]
//! implementations.
//!
//! Run these functions only against fresh, disposable stores. They mutate and
//! clean a reserved binary prefix but deliberately exercise real commits,
//! rollbacks, batches, cursors, and savepoints.

use super::KeyValueStore;
use crate::{StorageBackendError, StorageBackendResult};

const PREFIX: &[u8] = b"\0uqa-key-value-conformance/v1/";

/// Verify the single-session ordered Key/Value and transaction contract.
pub fn verify_store(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
    store.delete_prefix(PREFIX)?;
    store.put(&key(b"2"), b"two")?;
    store.put(&key(b"1"), b"one")?;
    expect_eq(
        &store.scan_prefix(PREFIX)?,
        &vec![(key(b"1"), b"one".to_vec()), (key(b"2"), b"two".to_vec())],
        "ordered prefix scan",
    )?;
    expect_eq(
        &store.scan_prefix_keys_after(PREFIX, Some(&key(b"1")), 1)?,
        &vec![key(b"2")],
        "bounded key cursor",
    )?;

    let mut batch = store.batch();
    batch.delete(&key(b"1"))?;
    batch.put(&key(b"3"), b"three")?;
    batch.commit()?;
    expect(store.get(&key(b"1"))?.is_none(), "batch delete")?;
    expect_eq(
        &store.get(&key(b"3"))?,
        &Some(b"three".to_vec()),
        "batch put",
    )?;

    store.begin_transaction()?;
    store.put(&key(b"before-savepoint"), b"kept")?;
    store.savepoint("uqa_contract")?;
    store.put(&key(b"after-savepoint"), b"discarded")?;
    store.rollback_to_savepoint("uqa_contract")?;
    expect(
        store.get(&key(b"after-savepoint"))?.is_none(),
        "savepoint rollback",
    )?;
    store.release_savepoint("uqa_contract")?;
    store.commit_transaction()?;
    expect_eq(
        &store.get(&key(b"before-savepoint"))?,
        &Some(b"kept".to_vec()),
        "outer transaction commit",
    )?;

    store.begin_transaction()?;
    store.put(&key(b"outer-rollback"), b"discarded")?;
    store.rollback_transaction()?;
    expect(
        store.get(&key(b"outer-rollback"))?.is_none(),
        "outer transaction rollback",
    )?;

    store.begin_read_transaction()?;
    if store.put(&key(b"read-only"), b"forbidden").is_ok() {
        expect(
            store.transaction_has_written()?,
            "read-first transaction write observation",
        )?;
    }
    store.rollback_transaction()?;
    store.delete_prefix(PREFIX)?;
    Ok(())
}

/// Verify MVCC visibility between two transaction-isolated sessions sharing
/// one physical database.
pub fn verify_session_isolation(
    reader: &dyn KeyValueStore,
    writer: &dyn KeyValueStore,
) -> StorageBackendResult<()> {
    writer.delete_prefix(PREFIX)?;
    writer.put(&key(b"isolation"), b"before")?;
    reader.begin_read_transaction()?;
    expect_eq(
        &reader.get(&key(b"isolation"))?,
        &Some(b"before".to_vec()),
        "initial reader snapshot",
    )?;
    writer.put(&key(b"isolation"), b"after")?;
    expect_eq(
        &reader.get(&key(b"isolation"))?,
        &Some(b"before".to_vec()),
        "pinned reader snapshot",
    )?;
    reader.commit_transaction()?;
    expect_eq(
        &reader.get(&key(b"isolation"))?,
        &Some(b"after".to_vec()),
        "post-commit reader visibility",
    )?;
    writer.delete_prefix(PREFIX)?;
    Ok(())
}

fn key(suffix: &[u8]) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    key.extend_from_slice(suffix);
    key
}

fn expect(condition: bool, contract: &str) -> StorageBackendResult<()> {
    if condition {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "KeyValue conformance failed: {contract}"
        )))
    }
}

fn expect_eq<T>(actual: &T, expected: &T, contract: &str) -> StorageBackendResult<()>
where
    T: PartialEq,
{
    expect(actual == expected, contract)
}
