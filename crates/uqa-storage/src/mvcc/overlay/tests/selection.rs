//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::KeyValueRead;
use crate::mvcc::{
    DatabaseId, MemoryVersionStore, MergedRecordSnapshot, RecordMetadata, RecordRead,
};

use super::super::run::read_counts;

fn spilled(control: &StorageReadControl) -> PrivateRecordChanges {
    let changes = PrivateRecordChanges::new(control.memory());
    let revision = PrivateRecordRevision::allocate().unwrap();
    let mut writer = SpilledRunWriter::new(4096, 4096 * 7, control.memory()).unwrap();
    for id in 0..4096 {
        writer
            .push(
                &key(id),
                expected(id),
                crate::mvcc::commit::RecordWriteKind::Canonical,
                revision,
                (id % 7 != 0).then_some(b"value".as_slice()),
                control,
            )
            .unwrap();
    }
    let run = writer.finish().unwrap().unwrap();
    let mut state = changes.owner.state.lock();
    state.runs = state
        .runs
        .with_run(run, 64 << 10, control.memory(), control)
        .unwrap();
    state.revision = Some(revision);
    drop(state);
    changes
}

fn merged(changes: &PrivateRecordChanges) -> MergedRecordSnapshot {
    let committed = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let control = StorageReadControl::with_limit(1 << 20);
    committed
        .commit(
            &[RecordWrite {
                key: b"z",
                expected: None,
                value: Some(b"committed"),
            }],
            &control,
        )
        .unwrap();
    MergedRecordSnapshot::new(
        std::sync::Arc::new(committed.snapshot().unwrap()),
        changes.snapshot().unwrap(),
    )
}

#[test]
fn selected_dense_metadata_decodes_each_spilled_entry_once_and_retains_undo_boundary() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let savepoint = StorageSavepointId::allocate();
    changes.savepoint(savepoint).unwrap();
    stage(&changes, &mut Model::new(), 15, None, &control);
    let view = merged(&changes);
    changes.rollback_to_savepoint(savepoint).unwrap();
    let read = StorageReadControl::with_limit(32 << 10);
    let baseline = control.memory().used();
    read_counts::take();
    let mut selected = view.selected(&read);
    assert_eq!(read_counts::take().entries, 0, "construction must be lazy");
    for id in 0..4096 {
        let metadata = selected.metadata(&key(id), &read).unwrap().unwrap();
        assert_eq!(metadata.revision, expected(id));
        assert_eq!(metadata.live, id % 7 != 0 && id != 15);
        assert_eq!(selected.metadata(&key(id), &read).unwrap(), Some(metadata));
    }
    assert_eq!(
        selected.metadata(b"z", &read).unwrap().unwrap().revision,
        Some(CommitSequence::from_u64(1))
    );
    assert!(selected.metadata(b"zz", &read).unwrap().is_none());
    assert_eq!(
        control.memory().used(),
        baseline,
        "exhaustion releases run caches"
    );
    let counts = read_counts::take();
    assert_eq!(counts.entries, 4096, "{counts:?}");
    assert_eq!(counts.values, 0);
    // A backwards request restarts on the captured root, including its deletion.
    for (selected_key, expected_value) in [
        (key(16), Some(b"value".as_slice())),
        (key(15), None),
        (key(16), Some(b"value".as_slice())),
        (b"j".to_vec(), None),
        (b"z".to_vec(), Some(b"committed".as_slice())),
    ] {
        selected
            .visit_value(&selected_key, &read, &mut |record| {
                assert_eq!(record.and_then(|record| record.value), expected_value);
                Ok(())
            })
            .unwrap();
    }
    drop(selected);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn sparse_selected_reads_seek_blocks_even_with_only_streaming_workspace() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let view = merged(&changes);
    let pressure = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let read = StorageReadControl::with_limit(4 << 10);
    read_counts::take();
    let mut selected = view.selected(&read);
    for id in [4, 2000, 4000, 4096] {
        assert_eq!(
            selected.metadata(&key(id), &read).unwrap(),
            (id < 4096).then_some(RecordMetadata {
                revision: expected(id),
                live: true
            }),
        );
    }
    let counts = read_counts::take();
    assert!(
        counts.entries > 0 && counts.entries < 1200,
        "sparse keys must skip whole blocks: {counts:?}"
    );
    assert_eq!(counts.values, 0);
    drop((selected, pressure));
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn interrupted_selection_releases_readers_and_cannot_skip_a_partially_consumed_key() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let view = merged(&changes);
    let baseline = control.memory().used();
    let read = StorageReadControl::with_limit(32 << 10);
    let mut selected = view.selected(&read);
    assert!(selected.metadata(&key(1), &read).unwrap().is_some());
    let empty = StorageReadControl::with_limit(0);
    assert!(matches!(
        selected.metadata(&key(2), &empty),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(control.memory().used(), baseline);
    assert!(selected.metadata(&key(2), &read).is_err());
    drop(selected);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn selected_value_batches_stop_before_producing_another_key() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let view = merged(&changes);
    let read = StorageReadControl::with_limit(32 << 10);
    read_counts::take();
    let mut produced = 0;
    let mut keys = std::iter::from_fn(|| {
        produced += 1;
        assert_eq!(
            produced, 1,
            "a stopped visitor must not produce a later key"
        );
        let mut bytes = BudgetedVec::new(read.memory());
        bytes.extend_from_slice(&key(1)).unwrap();
        Some(Ok(bytes))
    });
    let mut calls = 0;
    view.visit_values(&mut keys, &read, &mut |selected, record| {
        assert_eq!(selected, key(1));
        assert_eq!(record.unwrap().value, Some(b"value".as_slice()));
        calls += 1;
        Ok(false)
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(read_counts::take().values, 1);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn stopped_selected_batches_resume_without_redecoding_previous_entries() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let view = merged(&changes);
    let read = StorageReadControl::with_limit(32 << 10);
    let mut keys = (0..512).map(|id| {
        let mut bytes = BudgetedVec::new(read.memory());
        bytes.extend_from_slice(&key(id))?;
        Ok(bytes)
    });
    read_counts::take();
    let mut selected = view.selected(&read);
    for id in 0..512 {
        let mut calls = 0;
        selected
            .visit_values(&mut keys, &read, &mut |selected_key, record| {
                assert_eq!(selected_key, key(id));
                assert_eq!(
                    record.unwrap().value,
                    (id % 7 != 0).then_some(b"value".as_slice())
                );
                calls += 1;
                Ok(false)
            })
            .unwrap();
        assert_eq!(calls, 1);
    }
    let counts = read_counts::take();
    assert!(
        counts.entries <= 513,
        "resuming after hydration must retain the cursor: {counts:?}"
    );
    assert_eq!(counts.values, (0..512).filter(|id| id % 7 != 0).count());
    drop(selected);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn retained_key_value_batches_forward_selected_reads_and_preserve_input_order() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = spilled(&control);
    let view = merged(&changes);
    let read = StorageReadControl::with_limit(64 << 10);
    let adapter = RecordRead::new(&view, DatabaseId::from_bytes([7; 16]), &read);
    let retained = adapter.retain(&[b"k", b"z"]).unwrap();
    for provider in [&adapter as &dyn KeyValueRead, &*retained] {
        let mut keys = (0..512).map(|id| {
            let mut bytes = BudgetedVec::new(read.memory());
            bytes.extend_from_slice(&key(id))?;
            Ok(bytes)
        });
        read_counts::take();
        let mut id = 0;
        provider
            .visit_key_presence(&mut keys, &mut |selected, present| {
                assert_eq!(selected, key(id));
                assert_eq!(present, id % 7 != 0);
                id += 1;
                Ok(true)
            })
            .unwrap();
        assert_eq!(id, 512);
        let counts = read_counts::take();
        assert!(counts.entries <= 513, "{counts:?}");
        assert_eq!(counts.values, 0);

        let requested = [key(3), key(1), key(1), key(7), b"j".to_vec(), b"z".to_vec()];
        let mut keys = requested.iter().map(|key| {
            let mut bytes = BudgetedVec::new(read.memory());
            bytes.extend_from_slice(key)?;
            Ok(bytes)
        });
        let mut position = 0;
        provider
            .visit_values(&mut keys, &mut |selected, value| {
                assert_eq!(selected, requested[position]);
                assert_eq!(
                    value,
                    match position {
                        0..=2 => Some(b"value".as_slice()),
                        5 => Some(b"committed".as_slice()),
                        _ => None,
                    }
                );
                position += 1;
                Ok(true)
            })
            .unwrap();
        assert_eq!(position, requested.len());
    }
    drop(retained);
    assert_eq!(read.memory().used(), 0);
}
