//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{
    commit::RecordWriteKind, CommitSequence, PrivateRecordChanges, PrivateRecordRevision,
    RecordWrite,
};

#[test]
fn small_private_overlay_spills_merge_and_replay_without_enlarging_its_allowance() {
    const COUNT: u64 = 8_192;
    let control = StorageReadControl::with_limit(64 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    for id in (0..COUNT).rev() {
        changes
            .apply(
                &[RecordWrite {
                    key: &id.to_be_bytes(),
                    expected: Some(CommitSequence::from_u64(id + 1)),
                    value: Some(b"x"),
                }],
                &control,
            )
            .unwrap_or_else(|error| panic!("write {id}: {error}"));
    }
    assert!(!changes.owner.state.lock().runs.is_empty());
    let original = changes.snapshot().unwrap();
    for id in (0..COUNT).step_by(97) {
        changes
            .apply(
                &[RecordWrite {
                    key: &id.to_be_bytes(),
                    expected: Some(CommitSequence::from_u64(id + 1)),
                    value: (!id.is_multiple_of(2)).then_some(b"y"),
                }],
                &control,
            )
            .unwrap_or_else(|error| panic!("replace {id}: {error}"));
    }
    let latest = changes.snapshot().unwrap();
    let baseline = control.memory().used();
    let mut cursor = latest.cursor(&[], None, &control).unwrap();
    let mut next = 0_u64;
    while let Some(entry) = cursor.next(&control).unwrap() {
        assert_eq!(entry.key(), next.to_be_bytes());
        let write = entry.read(&control).unwrap();
        assert_eq!(write.expected(), Some(CommitSequence::from_u64(next + 1)));
        let expected: Option<&[u8]> = if next.is_multiple_of(97) {
            (!next.is_multiple_of(2)).then_some(b"y")
        } else {
            Some(b"x")
        };
        assert_eq!(write.value(), expected);
        next += 1;
    }
    assert_eq!(next, COUNT);
    drop(cursor);
    assert_eq!(control.memory().used(), baseline);
    for id in [0_u64, 97, COUNT - 1] {
        assert_eq!(
            original
                .get(&id.to_be_bytes(), &control)
                .unwrap()
                .unwrap()
                .value(),
            Some(b"x".as_slice()),
        );
    }
    assert!(control.memory().peak() <= control.memory().limit());
    drop((latest, original, changes));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn merging_sequential_runs_leaves_space_for_each_reader_values_and_output() {
    let control = StorageReadControl::with_limit(64 << 10);
    let mut runs = Vec::new();
    for shard in 0_u64..4 {
        let mut writer = SpilledRunWriter::new(1_024, 8_192, control.memory()).unwrap();
        for position in 0..1_024 {
            let id = position * 4 + shard;
            writer
                .push(
                    &id.to_be_bytes(),
                    None,
                    RecordWriteKind::Canonical,
                    PrivateRecordRevision::for_tests(),
                    Some(&[u8::try_from(shard).unwrap(); 64]),
                    &control,
                )
                .unwrap();
        }
        runs.push(Arc::new(writer.finish().unwrap().unwrap()));
    }
    let merged = Arc::new(merge(&runs, control.memory(), &control).unwrap());
    let baseline = control.memory().used();
    let mut cursor = merged.cursor(Bound::Unbounded);
    for id in 0_u64..4_096 {
        let entry = cursor.next(&control).unwrap().unwrap();
        assert_eq!(entry.key.bytes(), id.to_be_bytes());
        let value = merged.load_value(entry.value.unwrap(), &control).unwrap();
        assert_eq!(&value[..], [u8::try_from(id % 4).unwrap(); 64]);
    }
    assert!(cursor.next(&control).unwrap().is_none());
    assert_eq!(control.memory().used(), baseline);
    assert!(control.memory().peak() <= control.memory().limit());
    drop((cursor, merged, runs));
    assert_eq!(control.memory().used(), 0);
}
