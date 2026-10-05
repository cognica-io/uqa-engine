//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::*;
use crate::mvcc::overlay::run::read_counts;
use crate::mvcc::{CommitSequence, RecordWrite};
use crate::StorageSavepointId;

fn grouped(key: &[u8]) -> VersionResult<Option<&[u8]>> {
    if key.starts_with(b"!") {
        return Ok(None);
    }
    if key.starts_with(b"?") {
        return Err(VersionError::InvalidEncoding("invalid test scope"));
    }
    Ok(key.get(..1))
}

fn stage(changes: &PrivateRecordChanges, key: &[u8], control: &StorageReadControl) {
    changes
        .apply(
            &[RecordWrite {
                key,
                expected: None,
                value: Some(&[7; 512]),
            }],
            control,
        )
        .unwrap();
}

fn collect(
    snapshot: &PrivateRecordSnapshot,
    control: &StorageReadControl,
) -> BTreeMap<Vec<u8>, PrivateRecordRevision> {
    let mut result = BTreeMap::new();
    let mut after = None;
    loop {
        let page = snapshot
            .revision_scopes(after.as_deref(), 7, control)
            .unwrap();
        let Some(last) = page.last() else {
            return result;
        };
        after = Some(last.key().to_vec());
        for key in page.iter() {
            assert!(result.insert(key.key().to_vec(), key.revision()).is_none());
        }
    }
}

#[test]
fn summary_work_depends_on_scopes_instead_of_spilled_records() {
    let control = StorageReadControl::with_limit(1 << 20);
    let _unrelated = control.memory().reserve(300 << 10).unwrap();
    let changes = PrivateRecordChanges::with_revision_scope(control.memory(), Some(grouped));
    for id in 0..2048 {
        stage(&changes, format!("a{id:06}").as_bytes(), &control);
    }
    assert!(!changes.owner.state.lock().runs.is_empty());
    {
        let state = changes.owner.state.lock();
        let summary = state.scopes.as_ref().unwrap().changes.owner.state.lock();
        assert!(
            summary.runs.is_empty(),
            "replacing one scope must not accumulate resident size"
        );
        assert_eq!(summary.records.len(), 1);
    }
    let expected = changes.snapshot().unwrap().revision().unwrap();
    stage(&changes, b"!ignored", &control);
    let snapshot = changes.snapshot().unwrap();
    read_counts::take();
    for _ in 0..3 {
        assert_eq!(
            collect(&snapshot, &control),
            BTreeMap::from([(b"a".to_vec(), expected)])
        );
    }
    let reads = read_counts::take();
    assert_eq!(reads.entries, 0, "summary must not reopen the source runs");
    assert_eq!(reads.values, 0, "summary must not read source payloads");
}

#[test]
fn summaries_restore_undo_branches_and_keep_retained_views_and_forks() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::with_revision_scope(control.memory(), Some(grouped));
    stage(&changes, b"a1", &control);
    let retained = changes.snapshot().unwrap();
    let initial = collect(&retained, &control);
    let mark = StorageSavepointId::allocate();
    changes.savepoint(mark).unwrap();
    let fork = changes.fork();
    stage(&changes, b"a2", &control);
    let discarded = collect(&changes.snapshot().unwrap(), &control);
    assert!(discarded[b"a".as_slice()] > initial[b"a".as_slice()]);
    let undo = allocation_counter::measure(|| changes.rollback_to_savepoint(mark).unwrap());
    assert_eq!(undo.count_total, 0);
    assert_eq!(collect(&changes.snapshot().unwrap(), &control), initial);
    changes
        .apply(
            &[RecordWrite {
                key: b"a1",
                expected: None,
                value: None,
            }],
            &control,
        )
        .unwrap();
    let deleted = collect(&changes.snapshot().unwrap(), &control);
    assert!(deleted[b"a".as_slice()] > discarded[b"a".as_slice()]);
    stage(&fork, b"b1", &control);
    assert_eq!(collect(&retained, &control), initial);
    assert_eq!(collect(&fork.snapshot().unwrap(), &control).len(), 2);
    changes.release_savepoint(mark).unwrap();
    let undo = allocation_counter::measure(|| changes.rollback().unwrap());
    assert_eq!(undo.count_total, 0);
    assert!(collect(&changes.snapshot().unwrap(), &control).is_empty());
    stage(&changes, b"c1", &control);
    assert_eq!(collect(&changes.snapshot().unwrap(), &control).len(), 1);
    assert_eq!(collect(&retained.try_clone().unwrap(), &control), initial);
}

#[test]
fn summary_failures_publish_neither_records_nor_revisions() {
    let control = StorageReadControl::with_limit(128 << 10);
    let changes = PrivateRecordChanges::with_revision_scope(control.memory(), Some(grouped));
    stage(&changes, b"a1", &control);
    let before = changes.snapshot().unwrap();
    let expected = collect(&before, &control);
    for writes in [
        vec![
            RecordWrite {
                key: b"b1",
                expected: None,
                value: None,
            },
            RecordWrite {
                key: b"?invalid",
                expected: None,
                value: None,
            },
        ],
        vec![RecordWrite {
            key: b"a1",
            expected: Some(CommitSequence::from_u64(1)),
            value: None,
        }],
    ] {
        assert!(changes.apply(&writes, &control).is_err());
        assert_eq!(changes.snapshot().unwrap().revision(), before.revision());
        assert_eq!(collect(&changes.snapshot().unwrap(), &control), expected);
    }
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(changes
        .apply(
            &[RecordWrite {
                key: b"b1",
                expected: None,
                value: None
            }],
            &control
        )
        .is_err());
    drop(held);
    assert_eq!(collect(&changes.snapshot().unwrap(), &control), expected);
    assert!(changes
        .snapshot()
        .unwrap()
        .get(b"b1", &control)
        .unwrap()
        .is_none());
    control.cancellation().cancel();
    assert!(changes
        .apply(
            &[RecordWrite {
                key: b"b1",
                expected: None,
                value: None
            }],
            &control
        )
        .is_err());
    assert!(before.revision_scopes(None, 1, &control).is_err());
}

#[test]
fn high_cardinality_summaries_spill_and_match_exact_private_revisions() {
    let control = StorageReadControl::with_limit(256 << 10);
    let changes =
        PrivateRecordChanges::with_revision_scope(control.memory(), Some(|key| Ok(Some(key))));
    let mut expected = BTreeMap::new();
    for id in 0..2048 {
        let key = format!("{id:06}:{}", "k".repeat(128)).into_bytes();
        stage(&changes, &key, &control);
        expected.insert(key, changes.snapshot().unwrap().revision().unwrap());
    }
    {
        let state = changes.owner.state.lock();
        assert!(!state
            .scopes
            .as_ref()
            .unwrap()
            .changes
            .owner
            .state
            .lock()
            .runs
            .is_empty());
    }
    assert_eq!(collect(&changes.snapshot().unwrap(), &control), expected);
    let prepared = changes.prepare(&control).unwrap();
    assert_eq!(
        prepared.len(),
        expected.len(),
        "summaries must never enter publication"
    );
    let ungrouped = PrivateRecordChanges::new(control.memory());
    stage(&ungrouped, b"fallback", &control);
    let snapshot = ungrouped.snapshot().unwrap();
    assert_eq!(
        collect(&snapshot, &control),
        BTreeMap::from([(b"fallback".to_vec(), snapshot.revision().unwrap())])
    );
}
