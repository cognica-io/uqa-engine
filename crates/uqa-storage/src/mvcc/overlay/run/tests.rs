//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Bound;
use std::sync::Arc;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::{CommitSequence, PrivateRecordRevision};
use crate::read_control::StorageReadControl;

use super::{SpilledRun, SpilledRunWriter};

fn key(index: usize) -> Vec<u8> {
    format!("key-{index:06}").into_bytes()
}

fn value(index: usize) -> Option<Vec<u8>> {
    (!index.is_multiple_of(5)).then(|| {
        format!("value of {index} ")
            .repeat(index % 7 + 1)
            .into_bytes()
    })
}

fn expected(index: usize) -> Option<CommitSequence> {
    (!index.is_multiple_of(3)).then(|| CommitSequence::from_u64(index as u64 * 10))
}

/// A run of `count` changes at the even indexes, so that every odd index is a key between two of its keys.
fn run(count: usize, control: &StorageReadControl) -> (SpilledRun, Vec<PrivateRecordRevision>) {
    let mut writer =
        SpilledRunWriter::new(count as u64, count as u64 * 12, control.memory()).unwrap();
    let mut identities = Vec::new();
    for index in (0..count).map(|index| index * 2) {
        let identity = PrivateRecordRevision::for_tests();
        identities.push(identity);
        writer
            .push(
                &key(index),
                expected(index),
                RecordWriteKind::Canonical,
                identity,
                value(index).as_deref(),
                control,
            )
            .unwrap();
    }
    (writer.finish().unwrap().unwrap(), identities)
}

#[test]
fn point_reads_find_every_change_and_no_absent_key_across_blocks() {
    let control = StorageReadControl::with_limit(64 << 20);
    let (run, identities) = run(3000, &control);
    assert_eq!(run.len(), 3000);
    assert!(run.blocks.len() > 1, "{} blocks", run.blocks.len());
    for (position, identity) in identities.iter().enumerate() {
        let index = position * 2;
        let entry = run.get(&key(index), &control).unwrap().unwrap();
        assert_eq!(entry.key.bytes(), key(index).as_slice());
        assert_eq!(entry.expected, expected(index));
        assert_eq!(entry.identity, *identity);
        let loaded = entry
            .value
            .map(|location| run.load_value(location, &control).unwrap().to_vec());
        assert_eq!(loaded, value(index));
        assert!(run.get(&key(index + 1), &control).unwrap().is_none());
    }
    assert!(run.get(b"key-", &control).unwrap().is_none());
    assert!(run.get(b"key-999999", &control).unwrap().is_none());
    assert!(run.get(b"a", &control).unwrap().is_none());
}

#[test]
fn cursors_and_last_before_honor_their_bounds() {
    let control = StorageReadControl::with_limit(64 << 20);
    let (run, _) = run(2000, &control);
    let run = Arc::new(run);
    let keys = |start: Bound<&[u8]>| {
        let mut cursor = run.cursor(start);
        let mut keys = Vec::new();
        while let Some(entry) = cursor.next(&control).unwrap() {
            keys.push(entry.key.bytes().to_vec());
        }
        keys
    };
    let all = keys(Bound::Unbounded);
    assert_eq!(all.len(), 2000);
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(keys(Bound::Included(&key(1000))), all[500..].to_vec());
    assert_eq!(keys(Bound::Excluded(&key(1000))), all[501..].to_vec());
    assert_eq!(keys(Bound::Included(&key(1001))), all[501..].to_vec());
    assert!(keys(Bound::Excluded(&key(3998))).is_empty());
    let last = |end: Bound<&[u8]>| {
        run.last_before(end, &control)
            .unwrap()
            .map(|entry| entry.key.bytes().to_vec())
    };
    assert_eq!(last(Bound::Excluded(&key(1000))), Some(key(998)));
    assert_eq!(last(Bound::Included(&key(1000))), Some(key(1000)));
    assert_eq!(last(Bound::Excluded(&key(1001))), Some(key(1000)));
    assert_eq!(last(Bound::Excluded(&key(0))), None);
    assert_eq!(last(Bound::Unbounded), Some(key(3998)));
}

#[test]
fn a_run_without_room_for_its_filter_still_answers_exactly() {
    let control = StorageReadControl::with_limit(64 << 20);
    let mut writer = SpilledRunWriter::new(1 << 40, 64, control.memory()).unwrap();
    let identity = PrivateRecordRevision::for_tests();
    writer
        .push(
            b"b",
            None,
            RecordWriteKind::Canonical,
            identity,
            Some(b"v"),
            &control,
        )
        .unwrap();
    let run = writer.finish().unwrap().unwrap();
    assert!(run.filter.is_none());
    assert!(run.get(b"b", &control).unwrap().is_some());
    assert!(run.get(b"a", &control).unwrap().is_none());
    assert!(run.get(b"c", &control).unwrap().is_none());
}

#[test]
fn keys_must_increase() {
    let control = StorageReadControl::with_limit(64 << 20);
    let mut writer = SpilledRunWriter::new(2, 2, control.memory()).unwrap();
    let identity = PrivateRecordRevision::for_tests();
    writer
        .push(
            b"b",
            None,
            RecordWriteKind::Canonical,
            identity,
            None,
            &control,
        )
        .unwrap();
    assert!(writer
        .push(
            b"a",
            None,
            RecordWriteKind::Canonical,
            identity,
            None,
            &control
        )
        .is_err());
    assert!(writer
        .push(
            b"b",
            None,
            RecordWriteKind::Canonical,
            identity,
            None,
            &control
        )
        .is_err());
}

#[test]
fn decoded_run_cache_ends_with_active_readers_not_retained_roots() {
    let control = StorageReadControl::with_limit(256 * 1024);
    let (run, _) = run(400, &control);
    let run = Arc::new(run);
    let retained = Arc::clone(&run);
    let baseline = control.memory().used();
    let read = || {
        let entry = run.get(&key(2), &control).unwrap().unwrap();
        let loaded = run.load_value(entry.value.unwrap(), &control).unwrap();
        assert_eq!(&loaded[..], value(2).unwrap());
    };

    read();
    assert_eq!(
        control.memory().used(),
        baseline,
        "a point read retains no cache"
    );
    let reader = run.cache_reader();
    read();
    assert!(control.memory().used() > baseline);
    let mut cursor = run.cursor(Bound::Unbounded);
    assert!(cursor.next(&control).unwrap().is_some());
    drop(reader);
    assert!(
        control.memory().used() > baseline,
        "the cursor still uses the cache"
    );
    while cursor.next(&control).unwrap().is_some() {}
    assert_eq!(
        control.memory().used(),
        baseline,
        "exhaustion releases decoded blocks"
    );
    assert!(cursor.next(&control).unwrap().is_none());

    let mut partial = run.cursor(Bound::Unbounded);
    assert!(partial.next(&control).unwrap().is_some());
    read();
    drop(partial);
    assert_eq!(
        control.memory().used(),
        baseline,
        "early stop releases decoded blocks"
    );
    drop((run, retained));
    // The exhausted cursor retains only the immutable run until it is dropped.
    assert_eq!(control.memory().used(), baseline);
    drop(cursor);
    assert_eq!(control.memory().used(), 0);
}
