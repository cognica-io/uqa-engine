//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::KeyValueDiskANNPopulationRecords,
    mvcc::{
        overlay::run::{write_counts, SpilledRunWriter},
        MemoryVersionStore, PrivateRecordRevision,
    },
};

const VALUE: [u8; 1024] = [73; 1024];

fn prepared(count: u64, control: &StorageReadControl) -> PreparedRecordCommit {
    let mut writer = SpilledRunWriter::new(count + 1, count * 8 + 32, control.memory()).unwrap();
    let identity = PrivateRecordRevision::for_tests();
    for id in 0..count {
        writer
            .push(
                &id.to_be_bytes(),
                None,
                RecordWriteKind::Canonical,
                identity,
                Some(&VALUE),
                control,
            )
            .unwrap();
    }
    // A raw canonical vector invalidation selects population reconciliation even when the field has no published DiskANN population.
    let mut field = vec![b'v'];
    for name in ["items", "vector"] {
        field.extend_from_slice(&(name.len() as u32).to_be_bytes());
        field.extend_from_slice(name.as_bytes());
    }
    let mut origin = KeyValueDiskANNPopulationRecords
        .origin_prefix(&field, control)
        .unwrap()
        .to_vec();
    origin.extend_from_slice(&7_u64.to_be_bytes());
    writer
        .push(
            &origin,
            None,
            RecordWriteKind::DiskANNOrigin,
            identity,
            None,
            control,
        )
        .unwrap();
    PreparedRecordCommit::from_spilled_run(writer.finish().unwrap().unwrap(), control).unwrap()
}

#[test]
fn population_reconciliation_copies_sorted_spilled_values_only_once_per_output() {
    for count in [128, 512] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(128 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let current: Arc<dyn CommittedRecordSnapshot> = Arc::new(store.snapshot().unwrap());
            let original = prepared(count, &control);
            assert!(original.resident().is_none());
            write_counts::take();
            let resolved = reconcile(
                &original,
                &[],
                &current,
                &KeyValueDiskANNPopulationRecords,
                DatabaseId::from_bytes([7; 16]),
                mode,
                &control,
            )
            .unwrap();
            let written = write_counts::take();
            assert_eq!(written.bytes, 2 * count * VALUE.len() as u64, "{written:?}");
            assert_eq!(written.copied, count * VALUE.len() as u64, "{written:?}");
            let mut before = original.writes();
            let mut after = resolved.writes();
            for position in 0..=count {
                let before = before.next(&control).unwrap().unwrap();
                let after = after.next(&control).unwrap().unwrap();
                assert_eq!(after.key(), before.key());
                assert_eq!(after.expected(), before.expected());
                assert_eq!(after.value(), before.value());
                assert!(
                    after.kind()
                        == if position == count {
                            mode.kind(RecordWriteKind::DiskANNOrigin)
                        } else {
                            RecordWriteKind::Canonical
                        }
                );
            }
            assert!(before.next(&control).unwrap().is_none());
            assert!(after.next(&control).unwrap().is_none());
            control.cancellation().cancel();
            assert!(reconcile(
                &original,
                &[],
                &current,
                &KeyValueDiskANNPopulationRecords,
                DatabaseId::from_bytes([7; 16]),
                mode,
                &control
            )
            .is_err());
            drop((before, after));
            drop((resolved, original, current, store));
            assert_eq!(control.memory().used(), 0);
            assert!(control.memory().peak() <= control.memory().limit());
        }
    }
}
