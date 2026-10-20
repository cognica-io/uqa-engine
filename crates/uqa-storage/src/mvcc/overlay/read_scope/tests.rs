//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::overlay::run::{read_counts, SpilledRunWriter};
use crate::mvcc::{
    commit::RecordWriteKind, PrivateRecordChanges, PrivateRecordRevision, RecordWrite,
};

fn spilled_changes(control: &StorageReadControl) -> PrivateRecordChanges {
    let revision = PrivateRecordRevision::for_tests();
    let mut writer = SpilledRunWriter::new(512, 512 * 8, control.memory()).unwrap();
    for id in 0_u64..512 {
        writer
            .push(
                &id.to_be_bytes(),
                None,
                RecordWriteKind::Canonical,
                revision,
                Some(&[23; 1024]),
                control,
            )
            .unwrap();
    }
    PrivateRecordChanges::from_spilled_run(writer.finish().unwrap(), revision, None, control)
        .unwrap()
}

#[test]
fn private_snapshot_read_scopes_reuse_blocks_and_release_the_last_cache_owner() {
    let control = StorageReadControl::with_limit(128 * 1024);
    let changes = spilled_changes(&control);
    let view = changes.snapshot().unwrap();
    let run = view.runs.only_run().unwrap();
    let key = 1_u64.to_be_bytes();
    let read = || {
        assert_eq!(
            view.get(&key, &control).unwrap().unwrap().value(),
            Some(&[23; 1024][..])
        );
    };
    let baseline = control.memory().used();
    let before = run.read_io_counts();
    read_counts::take();
    for _ in 0..32 {
        read();
    }
    assert_eq!(read_counts::take().blocks, 32);
    let cold = run.read_io_counts();
    assert!(cold.0 - before.0 >= 32 && cold.1 - before.1 >= 32);
    assert_eq!(control.memory().used(), baseline);

    let first = view.read_scope(&control).unwrap();
    let second = view.read_scope(&control).unwrap();
    read();
    read_counts::take();
    let warm = run.read_io_counts();
    for _ in 0..32 {
        read();
    }
    assert_eq!(read_counts::take().blocks, 0);
    assert_eq!(run.read_io_counts(), warm);
    drop(first);
    read();
    assert_eq!(run.read_io_counts(), warm);
    drop(second);
    assert_eq!(control.memory().used(), baseline);
    read();
    assert!(run.read_io_counts().0 > warm.0);
    assert!(control.memory().peak() <= control.memory().limit());
    drop(view);
    drop(changes);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn private_snapshot_read_scopes_preserve_frozen_visibility_and_failed_admission() {
    let control = StorageReadControl::with_limit(128 * 1024);
    let changes = spilled_changes(&control);
    let view = changes.snapshot().unwrap();
    let scope = view.read_scope(&control).unwrap();
    let key = 1_u64.to_be_bytes();
    view.get(&key, &control).unwrap().unwrap();
    let baseline = control.memory().used();
    let warm = view.runs.only_run().unwrap().read_io_counts();
    assert!(view.read_scope(&StorageReadControl::with_limit(0)).is_err());
    assert_eq!(control.memory().used(), baseline);
    view.get(&key, &control).unwrap().unwrap();
    assert_eq!(view.runs.only_run().unwrap().read_io_counts(), warm);
    changes
        .apply(
            &[RecordWrite {
                key: &key,
                expected: None,
                value: Some(b"replacement"),
            }],
            &control,
        )
        .unwrap();
    assert_eq!(
        changes
            .snapshot()
            .unwrap()
            .get(&key, &control)
            .unwrap()
            .unwrap()
            .value(),
        Some(b"replacement".as_slice())
    );
    drop(changes);
    assert_eq!(
        view.get(&key, &control).unwrap().unwrap().value(),
        Some(&[23; 1024][..])
    );
    control.cancellation().cancel();
    assert!(view.get(&key, &control).is_err());
    assert!(view.read_scope(&control).is_err());
    drop(scope);
    drop(view);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= control.memory().limit());
}
