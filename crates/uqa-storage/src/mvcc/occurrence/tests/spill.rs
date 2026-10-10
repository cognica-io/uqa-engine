//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::overlay::run::{write_counts, SpilledRunWriter};
use std::collections::BTreeMap;

#[test]
fn structural_occurrence_resolution_writes_sorted_spill_once() {
    for count in [128, 512] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(128 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let base = store.snapshot().unwrap();
            let mut records = BTreeMap::new();
            for id in 0_u64..count {
                records.insert(
                    id.to_be_bytes().to_vec(),
                    (RecordWriteKind::Canonical, vec![73; 1024]),
                );
            }
            for (index, (key, value)) in keys(&control).into_iter().zip(payload()).enumerate() {
                records.insert(
                    key,
                    (
                        if index == 2 {
                            RecordWriteKind::Canonical
                        } else {
                            RecordWriteKind::Occurrence
                        },
                        value,
                    ),
                );
            }
            let bytes = records
                .values()
                .map(|(_, value)| value.len() as u64)
                .sum::<u64>();
            let key_bytes = records.keys().map(|key| key.len() as u64).sum();
            let mut writer =
                SpilledRunWriter::new(records.len() as u64, key_bytes, control.memory()).unwrap();
            for (key, (kind, value)) in &records {
                writer
                    .push(
                        key,
                        None,
                        *kind,
                        PrivateRecordRevision::for_tests(),
                        Some(value),
                        &control,
                    )
                    .unwrap();
            }
            let original =
                PreparedRecordCommit::from_spilled_run(writer.finish().unwrap().unwrap(), &control)
                    .unwrap();
            write_counts::take();
            let resolved = resolve(
                &original,
                &base,
                &base,
                &crate::key_value::KeyValueOccurrenceRecords,
                mode,
                &control,
            )
            .unwrap();
            let written = write_counts::take();
            assert_eq!(written.bytes, bytes, "{written:?}");
            assert_eq!(
                written.copied, 0,
                "an ordered replacement must not be merged again: {written:?}"
            );
            let mut cursor = resolved.writes();
            for (key, (_, value)) in &records {
                let write = cursor.next(&control).unwrap().unwrap();
                assert_eq!(write.key(), key);
                assert_eq!(write.value(), Some(value.as_slice()));
                assert_eq!(write.expected(), None);
                assert!(write.kind() == RecordWriteKind::Canonical);
            }
            assert!(cursor.next(&control).unwrap().is_none());
            drop(cursor);
            control.cancellation().cancel();
            assert!(resolve(
                &original,
                &base,
                &base,
                &crate::key_value::KeyValueOccurrenceRecords,
                mode,
                &control
            )
            .is_err());
            drop((resolved, original, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}
