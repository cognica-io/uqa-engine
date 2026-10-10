//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Filtering prepared records must not read values outside the selection.

use super::*;
use crate::mvcc::{
    commit::{PreparedLookup, PreparedRecordCommit, RecordWriteKind},
    overlay::run::{read_counts, SpilledRunWriter},
    CommitSequence, PrivateRecordRevision, RecordWrite, VersionError,
};

#[test]
fn metadata_selection_preserves_writes_without_loading_rejected_spill_values() {
    for spilled in [false, true] {
        let control = StorageReadControl::with_limit(4 << 20);
        let keys: Vec<_> = (0..16).map(|id| format!("key{id:02}")).collect();
        let large = vec![b'x'; 64 << 10];
        let writes: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(id, key)| RecordWrite {
                key: key.as_bytes(),
                expected: id.is_multiple_of(2).then(|| CommitSequence::from_u64(1)),
                value: if id == 6 {
                    None
                } else if id.is_multiple_of(2) {
                    Some(b"small".as_slice())
                } else {
                    Some(large.as_slice())
                },
            })
            .collect();
        let prepared = if spilled {
            let mut spill = SpilledRunWriter::new(16, 4096, control.memory()).unwrap();
            for write in &writes {
                spill
                    .push(
                        write.key,
                        write.expected,
                        RecordWriteKind::Canonical,
                        PrivateRecordRevision::for_tests(),
                        write.value,
                        &control,
                    )
                    .unwrap();
            }
            PreparedRecordCommit::from_spilled_run(spill.finish().unwrap().unwrap(), &control)
                .unwrap()
        } else {
            PreparedRecordCommit::new(&writes, &control).unwrap()
        };
        let read = StorageReadControl::with_limit(32 << 10);
        read_counts::take();
        let mut cursor = prepared.writes();
        let mut visited = 0;
        while let Some(write) = cursor
            .next_where(&read, |metadata| Ok(metadata.expected().is_some()))
            .unwrap()
        {
            let expected = &writes[visited * 2];
            assert_eq!(write.key(), expected.key);
            assert_eq!(write.value(), expected.value);
            assert_eq!(write.expected(), expected.expected);
            visited += 1;
        }
        assert_eq!(visited, 8);
        let counts = read_counts::take();
        assert_eq!(counts.values, if spilled { 7 } else { 0 });
        drop(cursor);
        assert_eq!(read.memory().used(), 0);
        let error = prepared
            .writes()
            .next_where(&read, |_| Err(VersionError::WrongDatabase));
        assert!(matches!(error, Err(VersionError::WrongDatabase)));
        assert_eq!(read_counts::take().values, 0);
        read.cancellation().cancel();
        assert!(prepared.writes().next_where(&read, |_| Ok(false)).is_err());
        assert_eq!(read.memory().used(), 0);
    }
}

#[test]
fn prepared_metadata_lookups_preserve_conditions_without_loading_values() {
    for spilled in [false, true] {
        let control = StorageReadControl::with_limit(2 << 20);
        let large = vec![b'x'; 128 << 10];
        let writes = [
            RecordWrite {
                key: b"deleted",
                expected: Some(CommitSequence::from_u64(7)),
                value: None,
            },
            RecordWrite {
                key: b"live",
                expected: None,
                value: Some(&large),
            },
        ];
        let prepared = if spilled {
            let mut spill = SpilledRunWriter::new(2, 11, control.memory()).unwrap();
            for write in &writes {
                spill
                    .push(
                        write.key,
                        write.expected,
                        RecordWriteKind::Occurrence,
                        PrivateRecordRevision::for_tests(),
                        write.value,
                        &control,
                    )
                    .unwrap();
            }
            PreparedRecordCommit::from_spilled_run(spill.finish().unwrap().unwrap(), &control)
                .unwrap()
        } else {
            PreparedRecordCommit::new(&writes, &control).unwrap()
        };
        let read = StorageReadControl::with_limit(4 << 10);
        let lookup = PreparedLookup::new(&prepared, &read).unwrap();
        read_counts::take();
        for write in &writes {
            let metadata = lookup.metadata(write.key, &read).unwrap().unwrap();
            assert_eq!(metadata.key(), write.key);
            assert_eq!(metadata.expected(), write.expected);
            assert_eq!(metadata.live(), write.value.is_some());
            assert_eq!(
                metadata.value_len(),
                write.value.map(|value| value.len() as u64)
            );
            assert!(
                metadata.kind()
                    == if spilled {
                        RecordWriteKind::Occurrence
                    } else {
                        RecordWriteKind::Canonical
                    }
            );
        }
        assert!(lookup.metadata(b"missing", &read).unwrap().is_none());
        assert_eq!(read_counts::take().values, 0);
        read.cancellation().cancel();
        assert!(lookup.metadata(b"live", &read).is_err());
        drop(lookup);
        drop(prepared);
        assert_eq!(read.memory().used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}
