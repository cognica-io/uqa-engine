//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn small_metadata_roots_keep_their_resident_capture_fast_path() {
    use super::super::run::write_counts;

    let control = StorageReadControl::with_limit(4096);
    let changes = PrivateRecordChanges::new(control.memory());
    changes
        .apply(
            &[
                RecordWrite {
                    key: b"a",
                    expected: None,
                    value: None,
                },
                RecordWrite {
                    key: b"c",
                    expected: None,
                    value: Some(b"private"),
                },
            ],
            &control,
        )
        .unwrap();
    write_counts::take();
    let view = changes.snapshot().unwrap();
    assert_eq!(write_counts::take().bytes, 0);
    let reader = StorageReadControl::with_limit(32);
    assert_eq!(view.get(b"a", &reader).unwrap().unwrap().value(), None);
    assert_eq!(
        view.get(b"c", &reader).unwrap().unwrap().value(),
        Some(&b"private"[..])
    );
    assert_eq!(reader.memory().used(), 0);
    drop((view, changes));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_root_sealing_remains_beneficial_under_small_allowances() {
    let control = StorageReadControl::with_limit(16 * 1024);
    let changes = PrivateRecordChanges::new(control.memory());
    changes
        .apply(
            &[RecordWrite {
                key: b"payload",
                expected: None,
                value: Some(&[7; 8 * 1024]),
            }],
            &control,
        )
        .unwrap();
    let view = changes.snapshot().unwrap();
    let peer = control.memory().reserve(12 * 1024).unwrap();
    drop(peer);
    assert_eq!(
        view.get(b"payload", &control).unwrap().unwrap().value(),
        Some(&[7; 8 * 1024][..])
    );
    drop((view, changes));
    assert_eq!(control.memory().used(), 0);
}

fn populated(control: &StorageReadControl) -> PrivateRecordChanges {
    let changes = PrivateRecordChanges::new(control.memory());
    for index in 0_u64..24 {
        changes
            .apply(
                &[RecordWrite {
                    key: &index.to_be_bytes(),
                    expected: None,
                    value: Some(&[index as u8; 16 * 1024]),
                }],
                control,
            )
            .unwrap();
    }
    changes
}

#[test]
fn current_point_reads_preserve_the_writable_tier_and_selected_payload_lease() {
    use super::super::run::write_counts;

    let control = StorageReadControl::with_limit(1 << 20);
    let changes = populated(&control);
    assert_eq!(spilled_runs(&changes), 0);
    let resident = changes.owner.state.lock().resident;
    let reader = StorageReadControl::with_limit(0);
    write_counts::take();
    for _ in 0..64 {
        assert_eq!(
            changes
                .get(&0_u64.to_be_bytes(), &reader)
                .unwrap()
                .unwrap()
                .value(),
            Some(&[0; 16 * 1024][..]),
        );
        assert!(changes.get(b"missing", &reader).unwrap().is_none());
    }
    assert_eq!(write_counts::take().bytes, 0);
    assert_eq!(spilled_runs(&changes), 0);
    assert_eq!(changes.owner.state.lock().resident, resident);
    let selected = changes.get(&0_u64.to_be_bytes(), &reader).unwrap().unwrap();
    changes.rollback().unwrap();
    assert_eq!(selected.value(), Some(&[0; 16 * 1024][..]));
    assert!(control.memory().used() >= 16 * 1024);
    assert_eq!(reader.memory().used(), 0);
    drop((selected, changes));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn current_point_reads_preserve_spilled_conditions_kinds_and_undo_visibility() {
    use crate::mvcc::commit::RecordWriteKind;

    let control = StorageReadControl::with_limit(64 * 1024);
    let changes = PrivateRecordChanges::with_memory_tier_limit(control.memory(), 0);
    let expected = Some(CommitSequence::from_u64(17));
    let original = PreparedRecordWrite::copy_bytes(b"row", expected, Some(b"original"), &control)
        .unwrap()
        .with_kind(RecordWriteKind::Marker);
    changes.apply_owned(&[original], &control).unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: b"peer",
                expected: None,
                value: Some(b"peer"),
            }],
            &control,
        )
        .unwrap();
    assert!(spilled_runs(&changes) > 0);
    let frozen = changes.snapshot().unwrap();
    let savepoint = StorageSavepointId::allocate();
    changes.savepoint(savepoint).unwrap();
    let deleted = PreparedRecordWrite::copy_bytes(b"row", expected, None, &control)
        .unwrap()
        .with_kind(RecordWriteKind::StatisticsMaintenance);
    changes.apply_owned(&[deleted], &control).unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: b"later",
                expected: None,
                value: Some(b"later"),
            }],
            &control,
        )
        .unwrap();
    let current = changes.get(b"row", &control).unwrap().unwrap();
    assert_eq!(current.key(), b"row");
    assert_eq!(current.expected(), expected);
    assert_eq!(current.value(), None);
    assert!(current.kind() == RecordWriteKind::StatisticsMaintenance);
    let prior = frozen.get(b"row", &control).unwrap().unwrap();
    assert_eq!(prior.value(), Some(&b"original"[..]));
    assert!(prior.kind() == RecordWriteKind::Marker);
    let too_small = StorageReadControl::with_limit(0);
    assert!(changes.get(b"row", &too_small).is_err());
    assert_eq!(too_small.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(64 * 1024);
    cancelled.cancellation().cancel();
    assert!(changes.get(b"row", &cancelled).is_err());
    changes.rollback_to_savepoint(savepoint).unwrap();
    let restored = changes.get(b"row", &control).unwrap().unwrap();
    assert_eq!(restored.expected(), expected);
    assert_eq!(restored.value(), Some(&b"original"[..]));
    assert!(restored.kind() == RecordWriteKind::Marker);
    drop((current, prior, restored, frozen, changes));
    assert_eq!(control.memory().used(), 0);
}

fn assert_original(view: &PrivateRecordSnapshot, control: &StorageReadControl) {
    for index in 0_u64..24 {
        assert_eq!(
            view.get(&index.to_be_bytes(), control)
                .unwrap()
                .unwrap()
                .value(),
            Some(&[index as u8; 16 * 1024][..]),
        );
    }
}

#[test]
fn command_snapshot_releases_large_resident_roots_for_the_next_producer() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = populated(&control);
    let frozen = changes.snapshot().unwrap();
    let revision = frozen.revision();
    let peer = control.memory().reserve(768 * 1024).unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: &0_u64.to_be_bytes(),
                expected: None,
                value: Some(b"replacement"),
            }],
            &control,
        )
        .unwrap();
    assert_original(&frozen, &control);
    assert_eq!(frozen.revision(), revision);
    assert_ne!(changes.snapshot().unwrap().revision(), revision);
    drop(changes);
    assert_original(&frozen, &control);
    drop(peer);
    drop(frozen);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn savepoint_releases_large_resident_roots_and_undo_preserves_their_values() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = populated(&control);
    let savepoint = StorageSavepointId::allocate();
    changes.savepoint(savepoint).unwrap();
    let peer = control.memory().reserve(768 * 1024).unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: &0_u64.to_be_bytes(),
                expected: None,
                value: None,
            }],
            &control,
        )
        .unwrap();
    changes.rollback_to_savepoint(savepoint).unwrap();
    assert_original(&changes.snapshot().unwrap(), &control);
    changes.release_savepoint(savepoint).unwrap();
    drop(peer);
    drop(changes);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_retained_root_capture_preserves_values_revisions_and_savepoint_state() {
    let control = StorageReadControl::with_limit(1 << 20);
    let changes = populated(&control);
    let revision = changes.owner.state.lock().revision;
    let savepoint = StorageSavepointId::allocate();
    let peer = control
        .memory()
        .reserve(control.memory().available() - 1)
        .unwrap();
    let held = control.memory().used();
    assert!(changes.snapshot().is_err());
    assert_eq!(control.memory().used(), held);
    assert!(changes.savepoint(savepoint).is_err());
    assert_eq!(control.memory().used(), held);
    assert!(matches!(
        changes.release_savepoint(savepoint),
        Err(VersionError::SavepointMissing(_))
    ));
    drop(peer);
    let frozen = changes.snapshot().unwrap();
    assert_eq!(frozen.revision(), revision);
    assert_original(&frozen, &control);
    drop(changes);
    drop(frozen);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_root_capture_never_rewrites_older_spilled_payloads() {
    use super::super::run::write_counts;

    let control = StorageReadControl::with_limit(1 << 20);
    let changes = PrivateRecordChanges::new(control.memory());
    let mut views = Vec::new();
    for shard in 0_u64..4 {
        for id in shard * 24..(shard + 1) * 24 {
            changes
                .apply(
                    &[RecordWrite {
                        key: &id.to_be_bytes(),
                        expected: None,
                        value: Some(&[id as u8; 16 * 1024]),
                    }],
                    &control,
                )
                .unwrap();
        }
        write_counts::take();
        views.push(changes.snapshot().unwrap());
        let written = write_counts::take();
        assert_eq!(written.bytes, 24 * 16 * 1024);
        assert_eq!(written.copied, 0);
    }
    assert_eq!(changes.owner.state.lock().runs.len(), 4);
    changes
        .apply(
            &[RecordWrite {
                key: &96_u64.to_be_bytes(),
                expected: None,
                value: Some(b"next producer"),
            }],
            &control,
        )
        .unwrap();
    assert_eq!(changes.owner.state.lock().runs.len(), 1);
    assert_eq!(write_counts::take().copied, 96 * 16 * 1024);
    for (shard, view) in views.iter().enumerate() {
        for id in 0..(shard as u64 + 1) * 24 {
            assert_eq!(
                view.get(&id.to_be_bytes(), &control)
                    .unwrap()
                    .unwrap()
                    .value(),
                Some(&[id as u8; 16 * 1024][..]),
            );
        }
        assert!(view.get(&96_u64.to_be_bytes(), &control).unwrap().is_none());
    }
    drop((changes, views));
    assert_eq!(control.memory().used(), 0);
}
