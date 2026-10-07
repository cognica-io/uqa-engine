//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation certificates name exactly the staged view and survive neither undo nor replacement.

use super::{expect, expect_eq, key, PREFIX};
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

/// Verify a provider implementing optional mutation certificates on a fresh disposable store. Unsupported providers should retain the safe default rather than run this capability check.
pub fn verify_mutation_revisions(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
    let selected = key(b"certified");
    verify_autocommit(store, &selected)?;
    store.begin_transaction()?;
    store.savepoint("before")?;
    let mut calls = 0;
    let first = store
        .with_mutation_revision(&[PREFIX], &mut |_, batch| {
            calls += 1;
            batch.put(&selected, b"first")
        })?
        .ok_or_else(|| StorageBackendError::Other("mutation certificate unavailable".into()))?;
    expect(calls == 1, "certified mutation evaluated once")?;
    let mut retained = None;
    store.with_read_view(&mut |read| {
        expect(
            first == read.revision(&[PREFIX])?,
            "certificate matches staged view",
        )?;
        expect_eq(
            &read.get(&selected)?.as_deref(),
            &Some(b"first".as_slice()),
            "certified value",
        )?;
        retained = Some(read.retain(&[PREFIX])?);
        Ok(())
    })?;
    let failed = store.with_mutation_revision(&[PREFIX], &mut |_, batch| {
        calls += 1;
        batch.put(&selected, b"failed")?;
        Err(StorageBackendError::Other(
            "injected evaluation failure".into(),
        ))
    });
    expect(
        calls == 2 && failed.is_err(),
        "failed mutation is neither replayed nor certified",
    )?;
    store.with_read_view(&mut |read| {
        expect(
            first == read.revision(&[PREFIX])?,
            "failed staging preserves identity",
        )?;
        expect_eq(
            &read.get(&selected)?.as_deref(),
            &Some(b"first".as_slice()),
            "failed staging preserves value",
        )
    })?;
    store.rollback_to_savepoint("before")?;
    store.with_read_view(&mut |read| {
        expect(
            first != read.revision(&[PREFIX])?,
            "undo rejects candidate identity",
        )?;
        expect(
            read.get(&selected)?.is_none(),
            "undo removes candidate value",
        )
    })?;
    let second = store
        .with_mutation_revision(&[PREFIX], &mut |_, batch| batch.put(&selected, b"second"))?
        .ok_or_else(|| StorageBackendError::Other("replacement certificate unavailable".into()))?;
    expect(
        first != second,
        "equal-position replacement has a distinct identity",
    )?;
    store.with_read_view(&mut |read| {
        expect(
            second == read.revision(&[PREFIX])?,
            "replacement certificate matches staged view",
        )
    })?;
    expect_eq(
        &retained.unwrap().get(&selected)?.as_deref(),
        &Some(b"first".as_slice()),
        "retained discarded branch",
    )?;
    store.release_savepoint("before")?;
    store.commit_transaction()?;
    expect_eq(
        &store.get(&selected)?.as_deref(),
        &Some(b"second".as_slice()),
        "certified mutation committed",
    )?;
    verify_transaction_undo(store, &selected)?;
    store.delete_prefix(PREFIX)?;
    Ok(())
}

fn verify_autocommit(store: &dyn KeyValueStore, selected: &[u8]) -> StorageBackendResult<()> {
    for value in [b"first", b"later"] {
        let mut calls = 0;
        let identity = store
            .with_mutation_revision(&[PREFIX], &mut |_, batch| {
                calls += 1;
                batch.put(selected, value)
            })?
            .ok_or_else(|| {
                StorageBackendError::Other("autocommit certificate unavailable".into())
            })?;
        expect(calls == 1, "autocommit evaluated once")?;
        store.with_read_view(&mut |read| {
            expect(
                identity == read.revision(&[PREFIX])?,
                "certificate names completed uncontended commit",
            )?;
            expect_eq(
                &read.get(selected)?.as_deref(),
                &Some(value.as_slice()),
                "certified autocommit value",
            )
        })?;
    }
    store.delete(selected)
}

/// Verify that an intervening independent commit prevents attribution of a private candidate to a newer committed view.
pub fn verify_mutation_revision_concurrency(
    writer: &dyn KeyValueStore,
    peer: &dyn KeyValueStore,
) -> StorageBackendResult<()> {
    let selected = key(b"candidate");
    let other = key(b"peer");
    let mut calls = 0;
    let identity = writer
        .with_mutation_revision(&[PREFIX], &mut |_, batch| {
            calls += 1;
            peer.put(&other, b"peer value")?;
            batch.put(&selected, b"candidate value")
        })?
        .ok_or_else(|| StorageBackendError::Other("private certificate unavailable".into()))?;
    expect(calls == 1, "intervening commit does not replay evaluation")?;
    writer.with_read_view(&mut |read| {
        expect(
            identity != read.revision(&[PREFIX])?,
            "private candidate cannot claim peer commit",
        )?;
        expect_eq(
            &read.get(&selected)?.as_deref(),
            &Some(b"candidate value".as_slice()),
            "own value committed",
        )?;
        expect_eq(
            &read.get(&other)?.as_deref(),
            &Some(b"peer value".as_slice()),
            "peer value remains visible",
        )
    })?;
    writer.delete_prefix(PREFIX)?;
    Ok(())
}

fn verify_transaction_undo(store: &dyn KeyValueStore, selected: &[u8]) -> StorageBackendResult<()> {
    store.begin_transaction()?;
    let discarded = store
        .with_mutation_revision(&[PREFIX], &mut |_, batch| batch.delete(selected))?
        .ok_or_else(|| StorageBackendError::Other("delete certificate unavailable".into()))?;
    store.rollback_transaction()?;
    store.with_read_view(&mut |read| {
        expect(
            discarded != read.revision(&[PREFIX])?,
            "transaction rollback rejects candidate",
        )?;
        expect_eq(
            &read.get(selected)?.as_deref(),
            &Some(b"second".as_slice()),
            "transaction rollback preserves committed value",
        )
    })?;
    Ok(())
}
