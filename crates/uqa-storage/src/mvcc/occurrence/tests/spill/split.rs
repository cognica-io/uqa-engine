//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn split_occurrence_windows_preserve_canonical_peer_validation() {
    for spilled in [false, true] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(256 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let base = crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
            let mut records = split_records(3, &control);
            for (key, (kind, _)) in &mut records {
                let address = OccurrenceAddress::decode(key).unwrap();
                if address.term == Some(b"term0001")
                    && address.projection == Some(OccurrenceProjection::Positions)
                {
                    *kind = RecordWriteKind::Canonical;
                }
            }
            let original = prepare_records(&records, spilled, &control);
            let resolved = resolve(
                &original,
                &base,
                &base,
                &crate::key_value::KeyValueOccurrenceRecords,
                mode,
                &control,
            )
            .unwrap();
            let mut cursor = resolved.writes();
            for (key, (kind, value)) in &records {
                let write = cursor.next(&control).unwrap().unwrap();
                let address = OccurrenceAddress::decode(key).unwrap();
                let kind = if address.term == Some(b"term0001") {
                    RecordWriteKind::Canonical
                } else {
                    *kind
                };
                assert_eq!(write.key(), key);
                assert_eq!(write.value(), Some(value.as_slice()));
                assert_eq!(write.expected(), None);
                assert!(write.kind() == mode.kind(kind));
            }
            assert!(cursor.next(&control).unwrap().is_none());
            assert_eq!(
                base.requests(),
                4 + usize::from(mode == ResolutionMode::Command)
            );
            drop(cursor);
            drop((resolved, original, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn split_occurrence_windows_release_buffered_scores_after_failure() {
    for spilled in [false, true] {
        for failure in ["score", "positions", "missing", "cancel"] {
            let control = StorageReadControl::with_limit(256 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let mut records = split_records(3, &control);
            let initial: Vec<_> = records
                .iter()
                .map(|(key, (_, value))| RecordWrite {
                    key,
                    expected: None,
                    value: Some(value),
                })
                .collect();
            store.commit(&initial, &control).unwrap();
            drop(initial);
            let selected = records
                .keys()
                .find(|key| {
                    let address = OccurrenceAddress::decode(key).unwrap();
                    address.term == Some(b"term0001")
                        && address.projection
                            == Some(if failure == "score" {
                                OccurrenceProjection::Score
                            } else {
                                OccurrenceProjection::Positions
                            })
                })
                .unwrap()
                .clone();
            if failure == "missing" {
                records.remove(&selected);
            } else if failure != "cancel" {
                records.get_mut(&selected).unwrap().1.pop();
            }
            let base = crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
            let base = if failure == "cancel" {
                base.cancelling_after(1)
            } else {
                base
            };
            let original = prepare_records(&records, spilled, &control);
            let retained = control.memory().used();
            assert!(
                resolve(
                    &original,
                    &base,
                    &base,
                    &crate::key_value::KeyValueOccurrenceRecords,
                    ResolutionMode::Publication,
                    &control
                )
                .is_err(),
                "{failure}"
            );
            assert_eq!(control.memory().used(), retained, "{failure}");
            drop((original, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}
