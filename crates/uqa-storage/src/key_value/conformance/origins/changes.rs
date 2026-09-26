//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current canonical changes, immutable mutation keys and retained undo branches on actual providers.

use super::{expect, expect_eq, values};
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::diskann_index::format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity};
use crate::key_value::vector_index::origin::journal;
use crate::key_value::{KeyValueDiskANNCanonical, KeyValueVectorIndex};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult, VectorIndex};
use std::sync::Arc;

fn index(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<KeyValueDiskANNCanonical> {
    KeyValueDiskANNCanonical::new(store.clone(), "diskann-changes", "embedding", 2)
}

/// Exercise canonical and journal cleanup without initializing any physical graph namespace. A deleted last field must still release its physical identities after the final reader closes.
pub fn verify_diskann_canonical_reclamation(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = index(store)?;
    let original = canonical.replace(0, &[vec![1.0, -0.0]], &control)?;
    let held = canonical.retain(&control)?;
    for document in 0..4 {
        canonical.replace(document, &[vec![0.0, 1.0]], &control)?;
    }
    let mut ordinary = KeyValueVectorIndex::new(store.clone(), "diskann-changes", "embedding", 2);
    ordinary.clear()?;
    store.reclaim_obsolete()?;
    expect_eq(
        &held.origin(0, &control)?,
        &Some(original),
        "retained canonical origin survives maintenance",
    )?;
    values(&held, 0, &[vec![1.0, -0.0]], &control)?;
    drop(held);
    store.reclaim_obsolete()?;
    expect_eq(
        &canonical.retain(&control)?.origin(0, &control)?,
        &None,
        "cleared canonical origin stays absent",
    )
}

pub(super) fn verify(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = index(store)?;
    let original = canonical.replace(0, &[vec![1.0, 0.0]], &control)?;
    let empty = canonical.replace(5, &[], &control)?;
    let last = canonical.replace(u64::MAX, &[vec![0.0, -0.0]], &control)?;
    let before = canonical.retain(&control)?;
    store.begin_transaction()?;
    store.savepoint("changes")?;
    let undone = canonical.replace(0, &[vec![-1.0, 0.0]], &control)?;
    let private = canonical.retain(&control)?;
    store.rollback_to_savepoint("changes")?;
    let mut current = original;
    for _ in 0..256 {
        current = canonical.replace(0, &[vec![0.0, 1.0]], &control)?;
    }
    store.release_savepoint("changes")?;
    store.commit_transaction()?;
    let query = StorageReadControl::with_limit(8192);
    for (source, first) in [
        (&before, original),
        (&private, undone),
        (&canonical.retain(&control)?, current),
    ] {
        let mut after = None;
        for (document, version) in [(0, first), (5, empty), (u64::MAX, last)] {
            let expected = DiskANNChangeIdentity::new(document, version);
            expect_eq(
                &source.next_change_after(after, &query)?,
                &Some(expected),
                "one visible change per document on the retained undo branch",
            )?;
            after = Some(document);
        }
        expect(
            source.next_change_after(after, &query)?.is_none(),
            "terminal change cursor is exhausted",
        )?;
    }
    let prefix = journal::prefix("diskann-changes", "embedding")?;
    let mut records = 0;
    store.with_read_view(&mut |read| {
        read.visit_keys_after(&prefix, None, usize::MAX, &query, &mut |_| {
            records += 1;
            Ok(())
        })
    })?;
    expect_eq(
        &records,
        &259,
        "rollback removes its journal entry and retries keep unique origins",
    )?;
    expect(
        records * (40 + 56) > query.memory().limit(),
        "journal exceeds query allowance",
    )?;
    expect_eq(&query.memory().used(), &0, "change scan releases workspace")?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        before.next_change_after(None, &tiny).is_err(),
        "change cursor respects query quota",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed cursor releases workspace",
    )?;
    let cancelled = StorageReadControl::with_limit(8192);
    cancelled.cancellation().cancel();
    expect(
        before
            .next_change_after(Some(u64::MAX), &cancelled)
            .is_err(),
        "empty cursor checks invoking cancellation",
    )?;
    malformed(store, &canonical, current, &control)?;
    ordinary_mutations(store, &canonical, &control)?;
    late_commit(store, &control)?;
    control.cancellation().cancel();
    expect(
        before.next_change_after(None, &query).is_err(),
        "change cursor preserves original cancellation",
    )
}

pub(super) fn verify_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(8192);
    let source = index(store)?.retain(&StorageReadControl::with_limit(1 << 20))?;
    let mut after = None;
    for document in [0, 5, u64::MAX] {
        let origin = source.origin(document, &control)?.expect("fixture origin");
        expect_eq(
            &source.next_change_after(after, &control)?,
            &Some(DiskANNChangeIdentity::new(document, origin)),
            "cold reopen preserves canonical change identity",
        )?;
        after = Some(document);
    }
    values(&source, 0, &[vec![0.0, 1.0]], &control)?;
    values(&source, 5, &[], &control)
}

fn malformed(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    current: DiskANNVectorVersion,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let key = journal::key(
        "diskann-changes",
        "embedding",
        DiskANNChangeIdentity::new(0, current),
    )?;
    for bad in [
        vec![0; 57],
        DiskANNCanonicalOrigin::new(current, 2, 2)?
            .encode()
            .to_vec(),
    ] {
        store.begin_transaction()?;
        store.put(&key, &bad)?;
        expect(
            canonical
                .retain(control)?
                .next_change_after(None, control)
                .is_err(),
            "malformed or inconsistent current changes fail",
        )?;
        store.rollback_transaction()?;
    }
    store.begin_transaction()?;
    let mut invalid_key = journal::prefix("diskann-changes", "embedding")?;
    invalid_key.extend_from_slice(&[0; 8]);
    store.put(&invalid_key, b"invalid")?;
    expect(
        canonical
            .retain(control)?
            .next_change_after(None, control)
            .is_err(),
        "malformed journal key fails",
    )?;
    store.rollback_transaction()?;
    Ok(())
}

fn late_commit(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let writer = store.open_session()?;
    let index = KeyValueDiskANNCanonical::new(writer.clone(), "late-change", "embedding", 2)?;
    writer.begin_transaction()?;
    let late = index.replace(7, &[vec![1.0, 0.0]], control)?;
    let other = KeyValueDiskANNCanonical::new(store.clone(), "late-change", "embedding", 2)?;
    let earlier_commit = other.replace(3, &[], control)?;
    let selected = other.retain(control)?;
    expect(
        late.writer().allocation() < earlier_commit.writer().allocation(),
        "late writer allocated first",
    )?;
    writer.commit_transaction()?;
    expect_eq(
        &selected.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(3, earlier_commit)),
        "captured changes exclude a later commit",
    )?;
    expect(
        selected.next_change_after(Some(3), control)?.is_none(),
        "retained view never advances to late commit",
    )?;
    expect_eq(
        &other.retain(control)?.next_change_after(Some(3), control)?,
        &Some(DiskANNChangeIdentity::new(7, late)),
        "fresh changes include the late commit regardless of allocation order",
    )
}

fn ordinary_mutations(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.begin_transaction()?;
    let mut ordinary = KeyValueVectorIndex::new(store.clone(), "diskann-changes", "embedding", 2);
    ordinary.add(0, vec![3.0, 4.0])?;
    expect(
        canonical
            .retain(control)?
            .next_change_after(None, control)
            .is_err(),
        "journal refuses unstamped canonical replacements",
    )?;
    ordinary.delete(0)?;
    expect_eq(
        &canonical
            .retain(control)?
            .next_change_after(None, control)?
            .map(DiskANNChangeIdentity::document),
        &Some(5),
        "deleted documents do not resurrect historical journal entries",
    )?;
    ordinary.clear()?;
    let prefix = journal::prefix("diskann-changes", "embedding")?;
    store.with_read_view(&mut |read| {
        expect(
            !read.contains_prefix_budgeted(&prefix, control)?,
            "ordinary field clear removes its change namespace",
        )
    })?;
    store.rollback_transaction()
}
