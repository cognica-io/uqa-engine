//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact revision reads preserve private identities without scanning unrelated spilled keys.

use super::*;
use crate::mvcc::{DatabaseId, MemoryVersionStore, MergedRecordSnapshot};

const DATABASE: DatabaseId = DatabaseId::from_bytes([19; 16]);

#[test]
fn committed_revision_does_not_read_unrelated_private_spill_blocks() {
    for count in [32, 128] {
        let control = StorageReadControl::with_limit(512 << 10);
        let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
        let sequence = store
            .commit(
                &[RecordWrite {
                    key: b"catalog",
                    expected: None,
                    value: Some(b"definition"),
                }],
                &control,
            )
            .unwrap();
        let changes = PrivateRecordChanges::new(control.memory());
        let mut model = Model::new();
        for id in 0..count {
            stage(
                &changes,
                &mut model,
                id,
                Some("x".repeat(64 << 10)),
                &control,
            );
        }
        assert!(spilled_runs(&changes) > 0);
        let view = MergedRecordSnapshot::new(
            Arc::new(store.snapshot().unwrap()),
            changes.snapshot().unwrap(),
        );
        run::read_counts::take();
        let revision = view
            .record_revision(DATABASE, b"catalog", &control)
            .unwrap()
            .unwrap();
        assert_eq!(revision.committed(DATABASE), Some(sequence));
        assert!(!revision.is_private());
        assert!(view
            .record_revision(DATABASE, b"absent", &control)
            .unwrap()
            .is_none());
        let reads = run::read_counts::take();
        assert_eq!(
            reads.entries, 0,
            "a point miss must not open a range cursor"
        );
        assert_eq!(reads.blocks, 0);
        assert_eq!(reads.values, 0);
        drop((view, changes));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn private_revision_point_reads_preserve_replacements_tombstones_and_undo() {
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
    let committed = Arc::new(store.snapshot().unwrap());
    let capture = || MergedRecordSnapshot::new(committed.clone(), changes.snapshot().unwrap());
    let retained = capture();
    let read = StorageReadControl::with_limit(4 << 10);
    let revision =
        |view: &MergedRecordSnapshot, id| view.record_revision(DATABASE, &key(id), &read).unwrap();
    run::read_counts::take();
    let original = revision(&retained, 0).unwrap();
    let one_read = run::read_counts::take();
    assert!(one_read.entries > 0);
    assert_eq!(one_read.values, 0);
    retained.metadata(&key(0), &read).unwrap();
    assert_eq!(run::read_counts::take().entries, one_read.entries);
    assert!(original.is_private());
    assert_eq!(original.committed_sequence(), expected(0));
    assert!(revision(&retained, 1).is_none());
    assert!(revision(&retained, 31).unwrap().is_private());
    let savepoint = StorageSavepointId::allocate();
    changes.savepoint(savepoint).unwrap();
    stage(&changes, &mut model, 0, None, &control);
    stage(
        &changes,
        &mut model,
        1,
        Some("replacement".into()),
        &control,
    );
    let deleted = capture();
    assert!(revision(&deleted, 0).is_none());
    assert!(revision(&deleted, 1).unwrap().is_private());
    changes.rollback_to_savepoint(savepoint).unwrap();
    assert_eq!(revision(&capture(), 0), Some(original));
    stage(
        &changes,
        &mut model,
        0,
        Some("x".repeat(64 << 10)),
        &control,
    );
    let replaced = capture();
    assert_ne!(revision(&replaced, 0), Some(original));
    assert_eq!(revision(&retained, 0), Some(original));
    assert!(revision(&deleted, 0).is_none());
    assert_eq!(run::read_counts::take().values, 0);
    read.cancellation().cancel();
    assert!(replaced.record_revision(DATABASE, &key(0), &read).is_err());
    drop((replaced, retained, deleted, changes));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn command_resolution_preserves_forwarded_revisions_and_advances_changed_markers() {
    use crate::mvcc::commit::RecordWriteKind;
    use crate::mvcc::resolution::ResolutionMode;

    for spill in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let source = if spill {
            PrivateRecordChanges::with_memory_tier_limit(control.memory(), 0)
        } else {
            PrivateRecordChanges::new(control.memory())
        };
        for (key, value, kind) in [
            (b"az", Some(&b"old"[..]), RecordWriteKind::Canonical),
            (b"aa", Some(&b"new"[..]), RecordWriteKind::Canonical),
            (b"ad", None, RecordWriteKind::Canonical),
            (b"mb", Some(&b"fixed"[..]), RecordWriteKind::Marker),
        ] {
            let write = PreparedRecordWrite::copy_bytes(key, None, value, &control)
                .unwrap()
                .with_kind(kind);
            source.apply_owned(&[write], &control).unwrap();
        }
        let original = source.snapshot().unwrap();
        let prepared = source.prepare(&control).unwrap();
        assert_eq!(prepared.resident().is_none(), spill);
        let store = MemoryVersionStore::new(control.memory());
        let revision = |snapshot: &PrivateRecordSnapshot, key: &[u8]| {
            snapshot
                .revision_metadata(key, &control)
                .unwrap()
                .unwrap()
                .1
        };
        for committed_key in [b"unrelated".as_slice(), b"mb"] {
            store
                .commit(
                    &[RecordWrite {
                        key: committed_key,
                        expected: None,
                        value: Some(b"fixed"),
                    }],
                    &control,
                )
                .unwrap();
            let current = store.snapshot().unwrap();
            let resolved = crate::mvcc::markers::resolve(
                &prepared,
                &current,
                ResolutionMode::Command,
                &control,
            )
            .unwrap();
            for independent in [false, true] {
                let separate = StorageReadControl::with_limit(1 << 20);
                let target = if independent { &separate } else { &control };
                let changes = PrivateRecordChanges::from_prepared(
                    &resolved,
                    Some(|key| Ok(key.get(..1))),
                    target,
                )
                .unwrap();
                let captured = changes.snapshot().unwrap();
                for key in [b"aa", b"ad", b"az"] {
                    assert_eq!(
                        revision(&captured, key),
                        revision(&original, key),
                        "{key:?}"
                    );
                }
                assert_eq!(
                    captured.scope_revision(b"a", target).unwrap(),
                    original.scope_revision(b"a", &control).unwrap(),
                );
                assert_eq!(
                    revision(&captured, b"mb") == revision(&original, b"mb"),
                    committed_key != b"mb",
                );
                let saved = StorageSavepointId::allocate();
                changes.savepoint(saved).unwrap();
                let same = changes.get(b"aa", target).unwrap().unwrap();
                changes.apply_owned(&[same], target).unwrap();
                assert_ne!(
                    revision(&changes.snapshot().unwrap(), b"aa"),
                    revision(&original, b"aa")
                );
                changes.rollback_to_savepoint(saved).unwrap();
                assert_eq!(
                    revision(&changes.snapshot().unwrap(), b"aa"),
                    revision(&original, b"aa")
                );
                drop((captured, changes));
                assert_eq!(separate.memory().used(), 0);
            }
        }
    }
}
