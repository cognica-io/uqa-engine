//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::*;
use crate::mvcc::CommitSequence;

type Model = BTreeMap<Vec<u8>, (Option<CommitSequence>, Option<Vec<u8>>)>;

mod cursors;
mod selection;

#[test]
fn forked_private_roots_share_prefix_and_undo_independently() {
    let control = StorageReadControl::with_limit(1 << 20);
    let original = PrivateRecordChanges::new(control.memory());
    let mut initial = Model::new();
    for index in 0..512 {
        stage(
            &original,
            &mut initial,
            index,
            Some("x".repeat(2048)),
            &control,
        );
    }
    assert!(spilled_runs(&original) > 0);
    let used = control.memory().used();
    let fork = original.fork();
    assert_eq!(control.memory().used(), used);
    let savepoint = StorageSavepointId::allocate();
    fork.savepoint(savepoint).unwrap();
    let mut forked = initial.clone();
    stage(&fork, &mut forked, 0, None, &control);
    stage(&fork, &mut forked, 512, Some("fork".into()), &control);
    let mut changed = initial.clone();
    stage(
        &original,
        &mut changed,
        1,
        Some("original".into()),
        &control,
    );
    assert_matches(&fork.snapshot().unwrap(), &forked, &control);
    assert_matches(&original.snapshot().unwrap(), &changed, &control);
    fork.rollback_to_savepoint(savepoint).unwrap();
    assert_matches(&fork.snapshot().unwrap(), &initial, &control);
    assert_matches(&original.snapshot().unwrap(), &changed, &control);
}

fn key(index: usize) -> Vec<u8> {
    format!("k{index:06}").into_bytes()
}

fn expected(index: usize) -> Option<CommitSequence> {
    index
        .is_multiple_of(2)
        .then(|| CommitSequence::from_u64(index as u64 + 1))
}

/// Stage one change of key `index` and record it in `model`.
fn stage(
    changes: &PrivateRecordChanges,
    model: &mut Model,
    index: usize,
    value: Option<String>,
    control: &StorageReadControl,
) {
    let write = PreparedRecordWrite::copy_bytes(
        &key(index),
        expected(index),
        value.as_deref().map(str::as_bytes),
        control,
    )
    .unwrap();
    changes.apply_owned(&[write], control).unwrap();
    model.insert(key(index), (expected(index), value.map(String::into_bytes)));
}

fn spilled_runs(changes: &PrivateRecordChanges) -> usize {
    changes.owner.state.lock().runs.len()
}

#[test]
fn metadata_reads_skip_spilled_values_and_preserve_live_deleted_and_shadowed_revisions() {
    use super::run::read_counts;
    use crate::mvcc::{MemoryVersionStore, MergedRecordSnapshot, RecordMetadata};

    let control = StorageReadControl::with_limit(512 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    for id in 0..32 {
        stage(
            &changes,
            &mut model,
            id,
            (id != 1).then(|| "x".repeat(64 << 10)),
            &control,
        );
    }
    assert!(spilled_runs(&changes) > 0);
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let committed = std::sync::Arc::new(store.snapshot().unwrap());
    let captured = MergedRecordSnapshot::new(committed.clone(), changes.snapshot().unwrap());
    let read = StorageReadControl::with_limit(4 << 10);
    read_counts::take();
    for (id, live) in [(0, true), (1, false), (31, true)] {
        assert_eq!(
            captured.metadata(&key(id), &read).unwrap(),
            Some(RecordMetadata {
                revision: expected(id),
                live
            })
        );
    }
    assert!(captured.metadata(b"missing", &read).unwrap().is_none());
    let counts = read_counts::take();
    assert!(
        counts.entries > 0,
        "metadata must have reached the spilled entry file"
    );
    assert_eq!(counts.values, 0, "metadata must not read the value file");
    assert!(
        captured.get(&key(0), &read).is_err(),
        "the payload exceeds this read allowance"
    );
    stage(&changes, &mut model, 0, None, &control);
    stage(&changes, &mut model, 1, Some("new".into()), &control);
    let current = MergedRecordSnapshot::new(committed, changes.snapshot().unwrap());
    assert!(!current.metadata(&key(0), &read).unwrap().unwrap().live);
    assert!(current.metadata(&key(1), &read).unwrap().unwrap().live);
    assert!(captured.metadata(&key(0), &read).unwrap().unwrap().live);
    assert!(!captured.metadata(&key(1), &read).unwrap().unwrap().live);
}

/// Every read of `snapshot` agrees with `model`.
fn assert_matches(snapshot: &PrivateRecordSnapshot, model: &Model, control: &StorageReadControl) {
    for (key, (expected, value)) in model {
        let write = snapshot.get(key, control).unwrap().unwrap();
        assert_eq!(write.expected(), *expected);
        assert_eq!(write.value(), value.as_deref());
    }
    assert!(snapshot.get(b"k", control).unwrap().is_none());
    assert!(snapshot.get(b"z", control).unwrap().is_none());
    // Pages of a bounded size, as readers take them.
    let mut expected_entries = model.iter();
    let mut after: Option<Vec<u8>> = None;
    loop {
        let page = snapshot.scan(b"k", after.as_deref(), 256, control).unwrap();
        if page.is_empty() {
            break;
        }
        for write in page.iter() {
            let (key, (expected, value)) = expected_entries.next().unwrap();
            assert_eq!(write.key(), key.as_slice());
            assert_eq!(write.expected(), *expected);
            assert_eq!(write.value(), value.as_deref());
        }
        after = page.last().map(|write| write.key().to_vec());
    }
    assert!(expected_entries.next().is_none());
    let after = model.keys().nth(model.len() / 2).unwrap();
    let keys = snapshot.scan_keys(b"k", Some(after), 10, control).unwrap();
    let expected_keys = model
        .keys()
        .filter(|key| *key > after)
        .take(10)
        .collect::<Vec<_>>();
    assert_eq!(
        keys.iter().map(PrivateRecordKey::key).collect::<Vec<_>>(),
        expected_keys
            .iter()
            .map(|key| key.as_slice())
            .collect::<Vec<_>>()
    );
    let last = snapshot
        .last_before(b"k", Some(after), control)
        .unwrap()
        .unwrap();
    assert_eq!(
        last.key(),
        model
            .range(..after.clone())
            .next_back()
            .unwrap()
            .0
            .as_slice()
    );
}

#[test]
fn resident_cursor_borrows_keys_without_allocating_merge_scratch() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    for key in [b"before".as_slice(), b"first", b"second"] {
        changes
            .apply(
                &[RecordWrite {
                    key,
                    expected: None,
                    value: Some(key),
                }],
                &control,
            )
            .unwrap();
    }
    let state = changes.owner.state.lock();
    let allocation = allocation_counter::measure(|| {
        let mut cursor = TieredCursor::new(
            Some(&state.records),
            &state.runs,
            std::ops::Bound::Included(b"first".as_slice()),
            &control,
        )
        .unwrap();
        for expected in [b"first".as_slice(), b"second"] {
            assert_eq!(cursor.next(&control).unwrap().unwrap().key(), expected);
        }
        assert!(cursor.next(&control).unwrap().is_none());
        assert!(cursor.next(&control).unwrap().is_none());
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_total, 0);
}

#[test]
fn shared_changes_keep_one_strong_counter_until_the_final_owner_drops() {
    let memory = MemoryBudget::new(1024);
    let bytes = size_of::<Owner>() + size_of::<usize>();
    let mut changes = None;
    let allocated = allocation_counter::measure(|| {
        changes = Some(PrivateRecordChanges::new(&memory));
    });
    assert_eq!(allocated.count_total, 1);
    assert_eq!(allocated.bytes_total, bytes as u64);
    let retained = changes.as_ref().unwrap().share_owner();
    let shared = allocation_counter::measure(|| drop(changes));
    assert_eq!(shared.count_total, 0);
    assert_eq!(shared.bytes_current, 0);
    let released = allocation_counter::measure(|| drop(retained));
    assert_eq!(released.count_current, -1);
    assert_eq!(released.bytes_current, -(bytes as i64));
}

#[test]
fn a_transaction_larger_than_its_allowance_spills_and_reads_its_newest_changes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    for index in 0..12_000 {
        stage(
            &changes,
            &mut model,
            index,
            Some(format!("first {index}")),
            &control,
        );
    }
    for index in (0..12_000).step_by(3) {
        stage(
            &changes,
            &mut model,
            index,
            Some(format!("second {index}")),
            &control,
        );
    }
    for index in (0..12_000).step_by(7) {
        stage(&changes, &mut model, index, None, &control);
    }
    assert!(spilled_runs(&changes) > 0);
    // Merging bounds the runs: fewer than FAN_IN in each size class.
    assert!(
        spilled_runs(&changes) < 12,
        "{} runs",
        spilled_runs(&changes)
    );
    assert!(control.memory().used() < control.memory().limit() / 2);
    assert_matches(&changes.snapshot().unwrap(), &model, &control);
    // The final changes of a spilled transaction are a spilled run, read one block at a time.
    let prepared = changes.prepare(&control).unwrap();
    assert!(prepared.resident().is_none());
    assert_eq!(prepared.len(), model.len());
    let mut writes = prepared.writes();
    for (key, (expected, value)) in &model {
        let write = writes.next(&control).unwrap().unwrap();
        assert_eq!(write.key(), key.as_slice());
        assert_eq!(write.expected(), *expected);
        assert_eq!(write.value(), value.as_deref());
    }
    assert!(writes.next(&control).unwrap().is_none());
}

#[test]
fn savepoints_and_command_views_keep_the_tiers_they_saw_across_spills() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    for index in 0..3_000 {
        stage(
            &changes,
            &mut model,
            index,
            Some(format!("before {index}")),
            &control,
        );
    }
    let saved = model.clone();
    let savepoint = StorageSavepointId::allocate();
    changes.savepoint(savepoint).unwrap();
    let view = changes.snapshot().unwrap();
    let runs = spilled_runs(&changes);
    for index in 0..9_000 {
        stage(
            &changes,
            &mut model,
            index,
            Some(format!("after {index}")),
            &control,
        );
    }
    assert!(spilled_runs(&changes) > runs);
    assert_matches(&view, &saved, &control);
    assert_matches(&changes.snapshot().unwrap(), &model, &control);
    changes.rollback_to_savepoint(savepoint).unwrap();
    assert_eq!(spilled_runs(&changes), runs);
    assert_matches(&changes.snapshot().unwrap(), &saved, &control);
    changes.rollback().unwrap();
    assert!(!changes.has_written());
}

#[test]
fn a_repeated_change_keeps_the_precondition_of_its_spilled_first_change() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    for index in 0..6_000 {
        stage(
            &changes,
            &mut model,
            index,
            Some(format!("value {index}")),
            &control,
        );
    }
    assert!(spilled_runs(&changes) > 0);
    let conflicting = PreparedRecordWrite::copy_bytes(
        &key(0),
        Some(CommitSequence::from_u64(99)),
        Some(b"other"),
        &control,
    )
    .unwrap();
    assert!(matches!(
        changes.apply_owned(&[conflicting], &control),
        Err(VersionError::WriteConflict { mutation: 0, .. })
    ));
    assert!(
        changes.write_kind(&key(0), &control).unwrap()
            == Some(super::super::commit::RecordWriteKind::Canonical)
    );
    stage(&changes, &mut model, 0, Some("again".into()), &control);
    assert_matches(&changes.snapshot().unwrap(), &model, &control);
}
