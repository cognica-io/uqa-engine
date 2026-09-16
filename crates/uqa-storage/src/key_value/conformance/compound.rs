//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed read boundaries and exactly-once mutation evaluation across providers.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

use super::{expect, expect_eq, key, PREFIX};

/// Verify that errors, unwinds, cancellation and read-only rejection do not apply an evaluated batch. Use a fresh disposable store.
pub fn verify_compound_mutations(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
    let kept = key(b"kept");
    let discarded = key(b"discarded");
    for in_transaction in [false, true] {
        store.delete_prefix(PREFIX)?;
        if in_transaction {
            store.begin_transaction()?;
        }
        store.put(&kept, b"original")?;
        for unwind in [false, true] {
            let mut calls = 0;
            let result = catch_unwind(AssertUnwindSafe(|| {
                store.with_mutation(&mut |read, batch| {
                    calls += 1;
                    expect_eq(
                        &read.get(&kept)?.as_deref(),
                        &Some(b"original".as_slice()),
                        "mutation reads prior writes",
                    )?;
                    batch.put(&kept, b"replaced")?;
                    batch.put(&discarded, b"discarded")?;
                    assert!(!unwind, "injected evaluated mutation unwind");
                    Err(StorageBackendError::Other(
                        "injected evaluated mutation error".into(),
                    ))
                })
            }));
            expect(calls == 1, "mutation evaluated exactly once")?;
            expect(
                if unwind {
                    result.is_err()
                } else {
                    matches!(result, Ok(Err(_)))
                },
                "evaluation failure propagated",
            )?;
            expect_eq(
                &store.get(&kept)?.as_deref(),
                &Some(b"original".as_slice()),
                "prior writes survive evaluation failure",
            )?;
            expect(
                store.get(&discarded)?.is_none(),
                "failed batch has no partial writes",
            )?;
        }

        let mut token = None;
        let cancelled = store.with_mutation(&mut |read, batch| {
            batch.put(&discarded, b"cancelled")?;
            token = Some(read.control().cancellation().clone());
            read.control().cancellation().cancel();
            Ok(())
        });
        let token =
            token.ok_or_else(|| StorageBackendError::Other("mutation was not evaluated".into()))?;
        let mut called = false;
        let pre_cancelled = store.with_mutation(&mut |_, _| {
            called = true;
            Ok(())
        });
        token.reset();
        expect(
            cancelled.is_err() && pre_cancelled.is_err() && !called,
            "cancellation rejects publication and later evaluation",
        )?;
        expect(
            store.get(&discarded)?.is_none(),
            "cancelled batch is discarded",
        )?;

        store.with_mutation(&mut |read, batch| {
            batch.put(&kept, b"success")?;
            expect_eq(
                &read.get(&kept)?.as_deref(),
                &Some(b"original".as_slice()),
                "batch staging does not advance the read view",
            )
        })?;
        if in_transaction {
            store.commit_transaction()?;
        }
        expect_eq(
            &store.get(&kept)?.as_deref(),
            &Some(b"success".as_slice()),
            "successful evaluated batch",
        )?;
    }
    store.begin_read_transaction()?;
    let mut called = false;
    let result = store.with_mutation(&mut |_, _| {
        called = true;
        Ok(())
    });
    store.rollback_transaction()?;
    expect(
        result.is_err() && !called,
        "read-only rejection precedes evaluation",
    )?;
    store.delete_prefix(PREFIX)?;
    Ok(())
}

/// Verify compound visibility and original write preconditions using two independent sessions over a fresh disposable database.
pub fn verify_compound_concurrency(
    a: &dyn KeyValueStore,
    b: &dyn KeyValueStore,
) -> StorageBackendResult<()> {
    let source = key(b"source");
    let derived = key(b"derived");
    b.put(&source, b"before")?;
    a.with_read_view(&mut |read| {
        let identity = read.revision(&[PREFIX])?;
        expect_eq(
            &read.get(&source)?.as_deref(),
            &Some(b"before".as_slice()),
            "initial compound value",
        )?;
        b.with_mutation(&mut |_, batch| {
            batch.put(&source, b"after")?;
            batch.put(&derived, b"after")
        })?;
        expect_eq(
            &read.get(&source)?.as_deref(),
            &Some(b"before".as_slice()),
            "compound point reads remain pinned",
        )?;
        let mut rows = Vec::new();
        read.visit_prefix(PREFIX, &mut |key, value| {
            rows.push((key.to_vec(), value.to_vec()));
            Ok(())
        })?;
        expect_eq(
            &rows,
            &vec![(source.clone(), b"before".to_vec())],
            "compound scan shares the point-read boundary",
        )?;
        expect(
            identity == read.revision(&[PREFIX])?,
            "compound cache identity remains pinned",
        )
    })?;
    expect_eq(
        &a.get(&source)?.as_deref(),
        &Some(b"after".as_slice()),
        "later reader observes new commit",
    )?;
    b.delete(&derived)?;
    let mut calls = 0;
    let result = a.with_mutation(&mut |read, batch| {
        calls += 1;
        expect_eq(
            &read.get(&source)?.as_deref(),
            &Some(b"after".as_slice()),
            "evaluation uses original snapshot",
        )?;
        b.put(&source, b"winner")?;
        expect_eq(
            &read.get(&source)?.as_deref(),
            &Some(b"after".as_slice()),
            "evaluation does not advance after concurrent commit",
        )?;
        batch.put(&source, b"loser")?;
        batch.put(&derived, b"must not publish")
    });
    expect(
        result.is_err() && calls == 1,
        "original precondition rejects conflict without reevaluation",
    )?;
    expect(
        a.in_transaction(),
        "failed publication retains evaluated attempt",
    )?;
    expect_eq(
        &b.get(&source)?.as_deref(),
        &Some(b"winner".as_slice()),
        "concurrent winner survives",
    )?;
    expect(
        b.get(&derived)?.is_none(),
        "conflicting derived batch remains private",
    )?;
    expect(
        a.commit_transaction().is_err(),
        "identical conflict remains rejected on commit retry",
    )?;
    a.rollback_transaction()?;
    expect_eq(
        &a.get(&source)?.as_deref(),
        &Some(b"winner".as_slice()),
        "rollback refreshes committed visibility",
    )?;
    a.delete_prefix(PREFIX)?;
    Ok(())
}
