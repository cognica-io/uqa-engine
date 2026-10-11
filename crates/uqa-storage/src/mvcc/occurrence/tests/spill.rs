//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::overlay::run::{read_counts, write_counts, SpilledRun, SpilledRunWriter};
use std::collections::BTreeMap;

type Records = BTreeMap<Vec<u8>, (RecordWriteKind, Vec<u8>)>;

fn spill(records: &Records, control: &StorageReadControl) -> Arc<SpilledRun> {
    let key_bytes = records.keys().map(|key| key.len() as u64).sum();
    let mut writer =
        SpilledRunWriter::new(records.len() as u64, key_bytes, control.memory()).unwrap();
    for (key, (kind, value)) in records {
        writer
            .push(
                key,
                None,
                *kind,
                PrivateRecordRevision::for_tests(),
                Some(value),
                control,
            )
            .unwrap();
    }
    Arc::new(writer.finish().unwrap().unwrap())
}

fn prepare_records(
    records: &Records,
    spilled: bool,
    control: &StorageReadControl,
) -> PreparedRecordCommit {
    if spilled {
        return PreparedRecordCommit::from_shared_spilled_run(spill(records, control), control)
            .unwrap();
    }
    let mut writes = BudgetedVec::new(control.memory());
    for (key, (kind, value)) in records {
        writes
            .push(
                PreparedRecordWrite::copy_bytes(key, None, Some(value), control)
                    .unwrap()
                    .with_kind(*kind),
            )
            .unwrap();
    }
    PreparedRecordCommit::from_unique_owned(writes, control).unwrap()
}

fn namespace_records(control: &StorageReadControl) -> Records {
    let mut records = BTreeMap::new();
    let values = payload();
    for table in ["alpha", "omega"] {
        for (key, value) in keys(control).into_iter().zip(&values) {
            let mut address = OccurrenceAddress::decode(&key).unwrap();
            address.table = table;
            records.insert(
                address.encode(control).unwrap().to_vec(),
                (RecordWriteKind::Occurrence, value.clone()),
            );
        }
    }
    records
}

#[test]
fn incremental_occurrence_validation_is_scoped_to_each_source() {
    for spilled in [false, true] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            for invalid_table in ["alpha", "omega"] {
                let control = StorageReadControl::with_limit(1 << 20);
                let store = MemoryVersionStore::new(control.memory());
                let base = store.snapshot().unwrap();
                let mut records = namespace_records(&control);
                let mut marker = OccurrenceAddress::table(invalid_table);
                marker.projection = Some(OccurrenceProjection::Format);
                records
                    .get_mut(&marker.encode(&control).unwrap().to_vec())
                    .unwrap()
                    .1 = b"invalid-format".to_vec();
                let original = prepare_records(&records, spilled, &control);
                assert!(matches!(
                    resolve(
                        &original,
                        &base,
                        &base,
                        &crate::key_value::KeyValueOccurrenceRecords,
                        mode,
                        &control,
                    ),
                    Err(VersionError::InvalidEncoding(
                        "invalid occurrence source marker"
                    ))
                ));
                drop((original, base, store));
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

#[test]
fn incremental_occurrence_validation_rechecks_fences_and_new_snapshots() {
    for spilled in [false, true] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(1 << 20);
            let store = MemoryVersionStore::new(control.memory());
            let base = store.snapshot().unwrap();
            let records = namespace_records(&control);
            let original = prepare_records(&records, spilled, &control);
            let layout = crate::key_value::KeyValueOccurrenceRecords;
            let resolved = resolve(&original, &base, &base, &layout, mode, &control).unwrap();
            drop(resolved);
            let mut marker = OccurrenceAddress::table("omega");
            marker.projection = Some(OccurrenceProjection::Format);
            let marker = marker.encode(&control).unwrap();
            let fence = layout
                .related_key(&marker, OccurrenceRelatedKey::Structure, &control)
                .unwrap();
            let changed = store
                .commit(
                    &[RecordWrite {
                        key: &fence,
                        expected: None,
                        value: Some(b"changed structure"),
                    }],
                    &control,
                )
                .unwrap();
            let current = store.snapshot().unwrap();
            assert!(matches!(
                resolve(&original, &base, &current, &layout, mode, &control),
                Err(VersionError::WriteConflict {
                    expected: None,
                    actual: Some(actual),
                    ..
                }) if actual == changed
            ));
            drop((original, marker, fence, current, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn incremental_occurrence_resolution_reads_spill_in_linear_work() {
    for count in [128, 512, 1024] {
        for advanced in [false, true] {
            for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
                let control = StorageReadControl::with_limit(2 << 20);
                let store = MemoryVersionStore::new(control.memory());
                let base = store.snapshot().unwrap();
                if advanced {
                    store
                        .commit(
                            &[RecordWrite {
                                key: b"unrelated",
                                expected: None,
                                value: Some(b"committed"),
                            }],
                            &control,
                        )
                        .unwrap();
                }
                let current = store.snapshot().unwrap();
                let values = payload();
                let mut records = BTreeMap::new();
                records.insert(
                    keys(&control).pop().unwrap(),
                    (RecordWriteKind::Occurrence, values[2].clone()),
                );
                for id in 0..count {
                    let term = format!("term{id:04}");
                    for (projection, value) in [
                        (OccurrenceProjection::Score, &values[0]),
                        (OccurrenceProjection::Positions, &values[1]),
                    ] {
                        let mut address = OccurrenceAddress::table("docs");
                        address.projection = Some(projection);
                        address.field = Some("body");
                        address.term = Some(term.as_bytes());
                        address.cluster = Some(0);
                        records.insert(
                            address.encode(&control).unwrap().to_vec(),
                            (RecordWriteKind::Occurrence, value.clone()),
                        );
                    }
                }
                let run = spill(&records, &control);
                let original =
                    PreparedRecordCommit::from_shared_spilled_run(run.clone(), &control).unwrap();
                let before = run.read_io_counts();
                let resolved = resolve(
                    &original,
                    &base,
                    &current,
                    &crate::key_value::KeyValueOccurrenceRecords,
                    mode,
                    &control,
                )
                .unwrap();
                let after = run.read_io_counts();
                let physical_blocks = run.file_blocks();
                let reads = after.0 - before.0 + after.1 - before.1;
                eprintln!(
                    "{count} terms, advanced={advanced}, publication={}: {reads} authenticated reads for {physical_blocks} source blocks",
                    mode == ResolutionMode::Publication,
                );
                assert!(
                    reads <= 8 * physical_blocks,
                    "{count} terms, advanced={advanced}, mode publication={}: {reads} authenticated reads for {physical_blocks} source blocks",
                    mode == ResolutionMode::Publication,
                );
                let mut cursor = resolved.writes();
                for (key, (_, value)) in &records {
                    let write = cursor.next(&control).unwrap().unwrap();
                    assert_eq!(write.key(), key);
                    assert_eq!(write.value(), Some(value.as_slice()));
                    assert_eq!(write.expected(), None);
                    assert!(
                        write.kind()
                            == if mode == ResolutionMode::Publication {
                                RecordWriteKind::Canonical
                            } else {
                                RecordWriteKind::Occurrence
                            }
                    );
                }
                assert!(cursor.next(&control).unwrap().is_none());
                drop(cursor);
                drop((resolved, original, run, current, base, store));
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

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

#[test]
fn structural_occurrence_resolution_reads_each_spilled_value_once() {
    for count in [4, 128, 512] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(128 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let base = store.snapshot().unwrap();
            let current = crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
            let values = payload();
            let mut records = BTreeMap::new();
            records.insert(
                keys(&control).pop().unwrap(),
                (RecordWriteKind::Canonical, values[2].clone()),
            );
            for id in 0..count {
                let term = format!("term{id:04}");
                for (projection, value) in [
                    (OccurrenceProjection::Score, &values[0]),
                    (OccurrenceProjection::Positions, &values[1]),
                ] {
                    let mut address = OccurrenceAddress::table("docs");
                    address.projection = Some(projection);
                    address.field = Some("body");
                    address.term = Some(term.as_bytes());
                    address.cluster = Some(0);
                    records.insert(
                        address.encode(&control).unwrap().to_vec(),
                        (RecordWriteKind::Occurrence, value.clone()),
                    );
                }
            }
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
            read_counts::take();
            let resolved = resolve(
                &original,
                &base,
                &current,
                &crate::key_value::KeyValueOccurrenceRecords,
                mode,
                &control,
            )
            .unwrap();
            current.assert_reads(usize::from(mode == ResolutionMode::Command), 0);
            assert_eq!(
                read_counts::take().values,
                records.len(),
                "a structural source needs its marker's kind, not another payload read per term"
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
            drop((resolved, original, current, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}
