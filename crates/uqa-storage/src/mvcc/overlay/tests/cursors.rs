//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{MemoryVersionStore, MergedRecordSnapshot, RecordMetadata};

fn merged(changes: &PrivateRecordChanges) -> MergedRecordSnapshot {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    MergedRecordSnapshot::new(
        std::sync::Arc::new(store.snapshot().unwrap()),
        changes.snapshot().unwrap(),
    )
}

#[test]
fn private_entry_cursor_preserves_retained_values_without_repeated_entry_scans() {
    let control = StorageReadControl::with_limit(512 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    for id in 0..256 {
        stage(
            &changes,
            &mut model,
            id,
            (id % 7 != 0).then(|| "x".repeat(4096)),
            &control,
        );
    }
    assert!(spilled_runs(&changes) > 0);
    let snapshot = changes.snapshot().unwrap();
    stage(&changes, &mut Model::new(), 16, None, &control);
    let read = StorageReadControl::with_limit(128 << 10);
    let retained_memory = control.memory().used();
    super::super::run::read_counts::take();
    let mut cursor = snapshot.cursor(b"k", Some(&key(15)), &read).unwrap();
    let mut entries = 0;
    let mut spilled = None;
    while let Some(entry) = cursor.next(&read).unwrap() {
        let id = entries + 16;
        assert_eq!(entry.key(), key(id));
        let write = entry.read(&read).unwrap();
        let (revision, value) = &model[entry.key()];
        assert_eq!(write.expected(), *revision);
        assert_eq!(write.value(), value.as_deref());
        assert_eq!(entry.metadata().live, value.is_some());
        assert!(entry.revision() <= snapshot.revision().unwrap());
        entries += 1;
        if spilled.is_none() && snapshot.records.get(entry.key()).is_none() {
            spilled = Some(entry);
        }
    }
    assert_eq!(entries, 240);
    assert!(cursor.next(&read).unwrap().is_none());
    assert_eq!(control.memory().used(), retained_memory);
    // An actual spilled entry still reads its selected value after all cursor readers close.
    let entry = spilled.unwrap();
    assert_eq!(
        entry.read(&read).unwrap().value(),
        model[entry.key()].1.as_deref()
    );
    drop(entry);
    let counts = super::super::run::read_counts::take();
    assert!(counts.entries > 0);
    assert!(counts.entries <= 512, "{counts:?}");
    drop(cursor);
    assert_eq!(read.memory().used(), 0);
    let mut outside = snapshot.cursor(b"k", Some(b"z"), &read).unwrap();
    assert!(outside.next(&read).unwrap().is_none());
    drop(outside);
    let mut prefix = snapshot.cursor(b"k00000", None, &read).unwrap();
    while prefix.next(&read).unwrap().is_some() {}
    assert_eq!(control.memory().used(), retained_memory);
    drop(prefix);

    let mut failed = snapshot.cursor(b"k", None, &read).unwrap();
    let exhausted = StorageReadControl::with_limit(0);
    assert!(failed.next(&exhausted).is_err());
    assert_eq!(control.memory().used(), retained_memory);
    assert!(
        failed.next(&read).is_err(),
        "an interrupted cursor cannot skip a consumed entry"
    );
    drop(failed);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn merged_key_scan_does_not_load_private_payloads_larger_than_its_allowance() {
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
    let captured = merged(&changes);
    stage(&changes, &mut model, 0, None, &control);
    stage(&changes, &mut model, 1, Some("new".into()), &control);
    let current = merged(&changes);
    let read = StorageReadControl::with_limit(32 << 10);
    super::super::run::read_counts::take();
    for (view, deleted) in [(&captured, 1), (&current, 0)] {
        let mut visited = 0;
        view.visit_keys(b"k", None, usize::MAX, &read, &mut |selected, metadata| {
            assert_eq!(selected, key(visited));
            assert_eq!(
                metadata,
                RecordMetadata {
                    revision: expected(visited),
                    live: visited != deleted,
                }
            );
            visited += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(visited, 32);
        assert_eq!(read.memory().used(), 0);
    }
    let counts = super::super::run::read_counts::take();
    assert!(counts.entries > 0);
    assert_eq!(counts.values, 0, "key scans must not load payloads");
}

#[test]
fn stopping_a_value_scan_does_not_load_the_next_private_payload() {
    let control = StorageReadControl::with_limit(512 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut model = Model::new();
    stage(&changes, &mut model, 0, Some("small".into()), &control);
    for id in 1..16 {
        stage(
            &changes,
            &mut model,
            id,
            Some("x".repeat(64 << 10)),
            &control,
        );
    }
    assert!(spilled_runs(&changes) > 0);
    let view = merged(&changes);
    let read = StorageReadControl::with_limit(32 << 10);
    for (limit, more) in [(1, true), (usize::MAX, false)] {
        super::super::run::read_counts::take();
        let mut calls = 0;
        view.visit_prefix(b"k", None, limit, &read, &mut |selected, record| {
            assert_eq!(selected, key(0));
            assert_eq!(record.value, Some(b"small".as_slice()));
            calls += 1;
            Ok(more)
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(super::super::run::read_counts::take().values, 1);
        assert_eq!(read.memory().used(), 0);
    }
}
