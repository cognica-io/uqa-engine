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
fn cursor_seeks_past_the_last_key_do_not_read_spilled_entries() {
    for count in [128, 1024] {
        let control = StorageReadControl::with_limit(128 << 10);
        let (run, _) = run(count, &control);
        let run = Arc::new(run);
        let baseline = control.memory().used();
        let last = key((count - 1) * 2);
        let beyond = key(count * 2);
        for start in [
            Bound::Included(beyond.as_slice()),
            Bound::Excluded(beyond.as_slice()),
            Bound::Excluded(last.as_slice()),
        ] {
            super::read_counts::take();
            let mut cursor = run.cursor(start);
            assert!(cursor.next(&control).unwrap().is_none());
            assert!(cursor.next(&control).unwrap().is_none());
            let reads = super::read_counts::take();
            assert_eq!(reads.blocks, 0);
            assert_eq!(reads.entries, 0);
            assert_eq!(reads.values, 0);
        }
        let mut cursor = run.cursor(Bound::Included(&last));
        assert_eq!(
            cursor.next(&control).unwrap().unwrap().key.bytes(),
            last.as_slice()
        );
        assert!(cursor.next(&control).unwrap().is_none());
        drop(cursor);
        let mut cursor = run.cursor(Bound::Unbounded);
        assert!(cursor.next(&control).unwrap().is_some());
        super::read_counts::take();
        assert!(cursor.seek_to(&beyond, &control).unwrap().is_none());
        assert!(cursor.next(&control).unwrap().is_none());
        let reads = super::read_counts::take();
        assert_eq!(reads.blocks, 0);
        assert_eq!(reads.entries, 0);
        assert_eq!(reads.values, 0);
        assert_eq!(control.memory().used(), baseline);
        control.cancellation().cancel();
        assert!(cursor.next(&control).is_err());
        drop((cursor, run));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn cursor_seeks_between_blocks_do_not_decode_preceding_entries() {
    let control = StorageReadControl::with_limit(128 << 10);
    let (run, _) = run(1024, &control);
    let run = Arc::new(run);
    assert!(run.blocks.len() > 1);
    let first = run.blocks[1].first.bytes().to_vec();
    let index = std::str::from_utf8(&first[4..])
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let missing = key(index - 1);
    let previous = key(index - 2);
    let baseline = control.memory().used();
    for start in [
        Bound::Included(missing.as_slice()),
        Bound::Excluded(missing.as_slice()),
        Bound::Excluded(previous.as_slice()),
    ] {
        super::read_counts::take();
        let mut cursor = run.cursor(start);
        assert_eq!(cursor.next(&control).unwrap().unwrap().key.bytes(), first);
        let reads = super::read_counts::take();
        assert_eq!(reads.blocks, 1, "{reads:?}");
        assert_eq!(reads.entries, 1, "{reads:?}");
        assert_eq!(reads.values, 0);
        drop(cursor);
        assert_eq!(control.memory().used(), baseline);
    }
    control.cancellation().cancel();
    assert!(run
        .cursor(Bound::Included(&missing))
        .next(&control)
        .is_err());
    drop(run);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unaligned_entry_ranges_decrypt_each_physical_block_once() {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut writer = SpilledRunWriter::new(1024, 10 * 1024, control.memory()).unwrap();
    for index in 0..1024 {
        writer
            .push(
                &key(index),
                None,
                RecordWriteKind::Canonical,
                PrivateRecordRevision::for_tests(),
                Some(b"value"),
                &control,
            )
            .unwrap();
    }
    let run = writer.finish().unwrap().unwrap();
    assert!(run
        .blocks
        .iter()
        .any(|block| block.offset % super::VALUE_CHUNK != 0));
    let baseline = control.memory().used();
    for (index, block) in run.blocks.iter().enumerate() {
        let before = run.entries.block_io_counts().0;
        let bytes = run.read_block(index, &control).unwrap();
        let expected = block.end.div_ceil(super::VALUE_CHUNK) - block.offset / super::VALUE_CHUNK;
        assert_eq!(bytes.len() as u64, block.end - block.offset);
        assert_eq!(
            run.entries.block_io_counts().0 - before,
            expected as usize,
            "entry block {index} at {}..{}",
            block.offset,
            block.end,
        );
        drop(bytes);
        assert_eq!(control.memory().used(), baseline);
    }
    control.cancellation().cancel();
    assert!(run.read_block(0, &control).is_err());
    drop(run);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unaligned_value_ranges_decrypt_each_physical_block_once() {
    let control = StorageReadControl::with_limit(256 << 10);
    let value: Vec<_> = (0..(2 * super::VALUE_CHUNK as usize + 19))
        .map(|index| (index % 251) as u8)
        .collect();
    let mut writer = SpilledRunWriter::new(2, 2, control.memory()).unwrap();
    for (key, bytes) in [(b"a", b"elevenbytes".as_slice()), (b"b", value.as_slice())] {
        writer
            .push(
                key,
                None,
                RecordWriteKind::Canonical,
                PrivateRecordRevision::for_tests(),
                Some(bytes),
                &control,
            )
            .unwrap();
    }
    let run = writer.finish().unwrap().unwrap();
    let entry = run.get(b"b", &control).unwrap().unwrap();
    let location = entry.value.unwrap();
    assert_eq!(location.offset, 11);
    let expected = (location.offset + location.len).div_ceil(super::VALUE_CHUNK)
        - location.offset / super::VALUE_CHUNK;
    let baseline = control.memory().used();
    let before = run.values.block_io_counts().0;
    let loaded = run.load_value(location, &control).unwrap();
    assert_eq!(&loaded[..], value.as_slice());
    assert_eq!(run.values.block_io_counts().0 - before, expected as usize);
    drop(loaded);
    assert_eq!(control.memory().used(), baseline);
    let before = run.values.block_io_counts().0;
    let mut copied = Vec::new();
    run.copy_value(
        location,
        &mut |bytes| {
            copied.extend_from_slice(bytes);
            Ok(())
        },
        &control,
    )
    .unwrap();
    assert_eq!(copied, value);
    assert_eq!(run.values.block_io_counts().0 - before, expected as usize);
    assert_eq!(control.memory().used(), baseline);
    drop((entry, run));
    assert_eq!(control.memory().used(), 0);
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
fn spill_writer_shares_small_remaining_ancestor_workspace_with_retained_input() {
    use uqa_core::memory::MemoryBudget;
    let memory = MemoryBudget::new(64 << 10);
    let retained_input = memory.reserve(40 << 10).unwrap();
    let child = memory.child(256 << 10);
    let control = StorageReadControl::new(&child, &uqa_core::CancellationToken::new());
    let (run, identities) = run(1_024, &control);
    assert!(run.blocks.len() > 1);
    let entries_path = run.entries.path().to_path_buf();
    let values_path = run.values.path().to_path_buf();
    for (position, identity) in identities.iter().enumerate() {
        let index = position * 2;
        let entry = run.get(&key(index), &control).unwrap().unwrap();
        assert_eq!(entry.identity, *identity);
        assert_eq!(entry.expected, expected(index));
        let bytes = entry
            .value
            .map(|location| run.load_value(location, &control).unwrap().to_vec());
        assert_eq!(bytes, value(index));
    }
    assert_eq!(memory.used(), retained_input.bytes() + child.used());
    assert!(memory.peak() <= memory.limit());
    drop(run);
    assert_eq!(memory.used(), retained_input.bytes());
    assert_eq!(child.used(), 0);
    assert!(!entries_path.exists());
    assert!(!values_path.exists());
}

#[test]
fn already_charged_entry_cache_does_not_require_new_reader_workspace() {
    let owner = StorageReadControl::with_limit(64 << 20);
    let (run, _) = run(1_024, &owner);
    let baseline = owner.memory().used();
    let retained = run.cache_reader();
    let first = super::reader::EntryReader::new(&run, 0, &owner).unwrap();
    assert!(owner.memory().used() > baseline);
    let reader = StorageReadControl::with_limit(0);
    let reused = super::reader::EntryReader::with_block_limit(&run, 0, 0, &reader).unwrap();
    assert!(matches!(reused, super::reader::EntryReader::Cached { .. }));
    assert_eq!(reader.memory().used(), 0);
    drop((first, reused, retained));
    assert_eq!(owner.memory().used(), baseline);
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
    assert_eq!(keys(Bound::Excluded(&key(3998))).len(), 0);
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

#[test]
fn pressured_run_reads_stream_entries_without_admitting_complete_blocks() {
    let control = StorageReadControl::with_limit(256 * 1024);
    let (run, identities) = run(600, &control);
    assert!(run.blocks.len() > 1);
    let path = run.entries.path().to_path_buf();
    let run = Arc::new(run);
    let used = control.memory().used();
    let pressure = control
        .memory()
        .reserve(control.memory().limit() - used - 2048)
        .unwrap();
    let baseline = control.memory().used();
    assert!(matches!(
        super::reader::EntryReader::new(&run, 0, &control).unwrap(),
        super::reader::EntryReader::Streaming(_)
    ));
    let entry = run.get(&key(700), &control).unwrap().unwrap();
    assert_eq!(entry.expected, expected(700));
    assert_eq!(entry.identity, identities[350]);
    assert_eq!(entry.kind.code(), RecordWriteKind::Canonical.code());
    assert!(entry.value.is_none());
    drop(entry);
    assert!(run.get(&key(701), &control).unwrap().is_none());
    assert_eq!(
        run.last_before(Bound::Excluded(&key(700)), &control)
            .unwrap()
            .unwrap()
            .key
            .bytes(),
        key(698)
    );

    let start = key(601);
    let mut cursor = run.cursor(Bound::Excluded(&start));
    control.cancellation().cancel();
    assert!(cursor.next(&control).is_err());
    control.cancellation().reset();
    for index in (602..1200).step_by(2) {
        let entry = cursor.next(&control).unwrap().unwrap();
        assert_eq!(entry.key.bytes(), key(index));
        assert_eq!(entry.expected, expected(index));
        assert_eq!(entry.identity, identities[index / 2]);
        if let Some(location) = entry.value {
            let bytes = run.load_value(location, &control).unwrap();
            assert_eq!(&bytes[..], value(index).unwrap());
        }
    }
    assert!(cursor.next(&control).unwrap().is_none());
    assert_eq!(control.memory().used(), baseline);
    drop(cursor);
    assert!(control.memory().peak() <= control.memory().limit());
    drop((pressure, run));
    assert_eq!(control.memory().used(), 0);
    assert!(!path.exists());
}

#[test]
fn cached_and_streamed_entries_preserve_metadata_and_corruption_errors() {
    use super::entry::{self, ValueLocation};
    let control = StorageReadControl::with_limit(2048);
    for code in 0..=11 {
        for expected in [None, Some(CommitSequence::from_u64(u64::MAX))] {
            for value in [
                None,
                Some(ValueLocation {
                    offset: u64::MAX - 8,
                    len: 8,
                }),
            ] {
                let mut bytes = Vec::new();
                entry::encode(
                    &mut bytes,
                    b"key",
                    expected,
                    RecordWriteKind::from_code(code).unwrap(),
                    PrivateRecordRevision::for_tests(),
                    value,
                )
                .unwrap();
                let raw = entry::decode(&bytes, &mut 0).unwrap();
                let read = |mut bytes: &[u8]| {
                    let mut remaining = bytes.len() as u64;
                    entry::read(&mut bytes, &mut remaining, &control)
                };
                let streamed = read(&bytes).unwrap();
                assert_eq!(streamed.key.bytes(), raw.key);
                assert_eq!(streamed.expected, raw.expected);
                assert_eq!(streamed.kind.code(), raw.kind.code());
                assert_eq!(streamed.identity, raw.identity);
                assert_eq!(streamed.value, raw.value);
                drop(streamed);
                for length in 0..bytes.len() {
                    assert_eq!(
                        entry::decode(&bytes[..length], &mut 0)
                            .err()
                            .unwrap()
                            .to_string(),
                        read(&bytes[..length]).err().unwrap().to_string()
                    );
                }
                let mut bad_flags = bytes.clone();
                bad_flags[7] = 4;
                assert_eq!(
                    entry::decode(&bad_flags, &mut 0).err().unwrap().to_string(),
                    read(&bad_flags).err().unwrap().to_string()
                );
            }
        }
    }
    assert_eq!(control.memory().used(), 0);
}
