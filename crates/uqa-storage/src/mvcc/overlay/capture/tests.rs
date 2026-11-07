//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{
    overlay::run::{read_counts, write_counts},
    RecordWrite,
};

fn put(
    source: &PrivateRecordChanges,
    key: &[u8],
    value: Option<&[u8]>,
    control: &StorageReadControl,
) {
    source
        .apply(
            &[RecordWrite {
                key,
                expected: None,
                value,
            }],
            control,
        )
        .unwrap();
}

#[test]
fn native_record_capture_retains_roots_without_spill_or_value_reads() {
    for tier in [0, 8 << 20] {
        let write = StorageReadControl::with_limit(16 << 20);
        let source = PrivateRecordChanges::with_memory_tier_limit(write.memory(), tier);
        for id in 0_u64..256 {
            put(&source, &id.to_be_bytes(), Some(&[7; 2048]), &write);
        }
        let (resident, runs, revision) = {
            let state = source.owner.state.lock();
            (state.records.len(), state.runs.len(), state.revision)
        };
        assert_eq!(runs > 0, tier == 0);
        read_counts::take();
        write_counts::take();
        let capture = StorageReadControl::with_limit(16 << 20);
        let mut snapshot = None;
        let allocated = allocation_counter::measure(|| {
            snapshot = Some(source.capture_native_records(&capture).unwrap());
        });
        assert!(allocated.bytes_total < 2048, "{allocated:?}");
        let reads = read_counts::take();
        let writes = write_counts::take();
        assert_eq!(
            (
                reads.blocks,
                reads.entries,
                reads.values,
                writes.bytes,
                writes.copied
            ),
            (0, 0, 0, 0, 0)
        );
        {
            let state = source.owner.state.lock();
            assert_eq!(
                (state.records.len(), state.runs.len(), state.revision),
                (resident, runs, revision)
            );
        }
        let retained = capture.memory().used();
        let exact = StorageReadControl::with_limit(retained);
        drop(source.capture_native_records(&exact).unwrap());
        assert_eq!(exact.memory().used(), 0);
        let denied = StorageReadControl::with_limit(retained - 1);
        assert!(matches!(
            source.capture_native_records(&denied),
            Err(VersionError::Memory(_))
        ));
        assert_eq!(denied.memory().used(), 0);
        let snapshot = snapshot.unwrap();
        put(&source, &0_u64.to_be_bytes(), None, &write);
        drop(source);
        let read = StorageReadControl::with_limit(64 << 10);
        for id in 0_u64..256 {
            let value = snapshot.get(&id.to_be_bytes(), &read).unwrap().unwrap();
            assert_eq!(value.value(), Some(&[7; 2048][..]));
        }
        assert_eq!(snapshot.revision(), revision);
        drop(snapshot);
        assert_eq!(capture.memory().used(), 0);
        assert_eq!(read.memory().used(), 0);
        assert_eq!(write.memory().used(), 0);
    }
}

#[test]
fn native_record_capture_charges_resident_payloads_and_preserves_undo_footprints() {
    let write = StorageReadControl::with_limit(16 << 20);
    let source = PrivateRecordChanges::with_memory_tier_limit(write.memory(), 8 << 20);
    put(&source, b"payload", Some(&vec![9; 2 << 20]), &write);
    let capture = StorageReadControl::with_limit(4 << 20);
    let original = source.capture_native_records(&capture).unwrap();
    let original_bytes = capture.memory().used();
    assert!(original_bytes >= 2 << 20);
    let denied = StorageReadControl::with_limit(1 << 20);
    assert!(matches!(
        source.capture_native_records(&denied),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    put(&source, b"payload", Some(b"short"), &write);
    let savepoint = crate::StorageSavepointId::allocate();
    source.savepoint(savepoint).unwrap();
    let small = StorageReadControl::with_limit(4096);
    let before = source.capture_native_records(&small).unwrap();
    let small_bytes = small.memory().used();
    put(&source, b"payload", Some(&vec![8; 1 << 20]), &write);
    assert!(source.capture_native_records(&denied).is_err());
    source.rollback_to_savepoint(savepoint).unwrap();
    let after = source.capture_native_records(&small).unwrap();
    assert_eq!(small.memory().used(), 2 * small_bytes);
    assert_eq!(
        after.get(b"payload", &small).unwrap().unwrap().value(),
        Some(&b"short"[..])
    );
    assert_eq!(
        original
            .get(b"payload", &capture)
            .unwrap()
            .unwrap()
            .value()
            .unwrap()
            .len(),
        2 << 20
    );
    source.rollback().unwrap();
    let empty = source.capture_native_records(&small).unwrap();
    assert!(empty.get(b"payload", &small).unwrap().is_none());
    drop((source, original, before, after, empty));
    assert_eq!(capture.memory().used(), 0);
    assert_eq!(small.memory().used(), 0);
    assert_eq!(write.memory().used(), 0);
}
