//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

/// A provider fixture that keeps score and positions in one row, like native `SQLite`.
struct Combined;

impl OccurrenceRecordLayout for Combined {
    fn kind(&self, key: &[u8], _: &StorageReadControl) -> VersionResult<OccurrenceRecordKind> {
        match key.first() {
            Some(b'c') => Ok(OccurrenceRecordKind::Cluster(0)),
            Some(b'm') => Ok(OccurrenceRecordKind::Format),
            _ => Err(VersionError::InvalidEncoding("invalid fixture key")),
        }
    }

    fn related_key(
        &self,
        _: &[u8],
        related: OccurrenceRelatedKey,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(match related {
            OccurrenceRelatedKey::Structure => b"f",
            OccurrenceRelatedKey::Format => b"m",
            OccurrenceRelatedKey::Skips => b"s",
            OccurrenceRelatedKey::BlockMax => b"b",
            _ => return Err(VersionError::InvalidEncoding("invalid fixture projection")),
        })?;
        Ok(key)
    }

    fn decode<'a>(
        &self,
        key: &[u8],
        value: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<OccurrenceRecordValue<'a>> {
        if self.kind(key, control)? == OccurrenceRecordKind::Format {
            return Ok(OccurrenceRecordValue::Format(value));
        }
        let length = value
            .get(..4)
            .ok_or(VersionError::InvalidEncoding("short fixture cluster"))?;
        let split = 4 + u32::from_be_bytes(length.try_into().unwrap()) as usize;
        if split > value.len() {
            return Err(VersionError::InvalidEncoding("short fixture score"));
        }
        Ok(OccurrenceRecordValue::Cluster {
            score: &value[4..split],
            positions: &value[split..],
        })
    }

    fn encode(
        &self,
        _: &[u8],
        _: &[u8],
        value: OccurrenceRecordValue<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut bytes = BudgetedVec::new(control.memory());
        match value {
            OccurrenceRecordValue::Cluster { score, positions } => {
                bytes.extend_from_slice(&(score.len() as u32).to_be_bytes())?;
                bytes.extend_from_slice(score)?;
                bytes.extend_from_slice(positions)?;
            }
            OccurrenceRecordValue::Format(value) => bytes.extend_from_slice(value)?,
            _ => return Err(VersionError::InvalidEncoding("invalid fixture value")),
        }
        Ok(bytes)
    }
}

fn records(count: usize, control: &StorageReadControl) -> Records {
    let values = payload();
    let cluster = Combined
        .encode(
            b"c",
            &[],
            OccurrenceRecordValue::Cluster {
                score: &values[0],
                positions: &values[1],
            },
            control,
        )
        .unwrap();
    let mut records = Records::new();
    for id in 0..count {
        records.insert(
            format!("c{id:05}").into_bytes(),
            (RecordWriteKind::Occurrence, cluster.to_vec()),
        );
    }
    records.insert(
        b"m".to_vec(),
        (RecordWriteKind::Occurrence, b"occurrences-v2".to_vec()),
    );
    records
}

fn verify(
    records: &Records,
    resolved: &PreparedRecordCommit,
    mode: ResolutionMode,
    control: &StorageReadControl,
) {
    let mut cursor = resolved.writes();
    for (key, (kind, value)) in records {
        let write = cursor.next(control).unwrap().unwrap();
        assert_eq!(write.key(), key);
        assert_eq!(write.value(), Some(value.as_slice()));
        assert_eq!(write.expected(), None);
        assert!(write.kind() == mode.kind(*kind));
    }
    assert!(cursor.next(control).unwrap().is_none());
}

#[test]
fn combined_occurrence_resolution_writes_ordered_spill_once() {
    for count in [128, 512, 1024] {
        for advanced in [false, true] {
            for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
                let control = StorageReadControl::with_limit(256 << 10);
                let store = MemoryVersionStore::new(control.memory());
                let base =
                    crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
                let current = advanced.then(|| {
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
                    store.snapshot().unwrap()
                });
                let latest: &dyn CommittedRecordSnapshot = match &current {
                    Some(current) => current,
                    None => &base,
                };
                let records = records(count, &control);
                let bytes = records
                    .values()
                    .map(|(_, value)| value.len() as u64)
                    .sum::<u64>();
                let run = spill(&records, &control);
                let original =
                    PreparedRecordCommit::from_shared_spilled_run(run.clone(), &control).unwrap();
                let before = run.read_io_counts();
                write_counts::take();
                let resolved =
                    resolve(&original, &base, latest, &Combined, mode, &control).unwrap();
                let written = write_counts::take();
                eprintln!(
                    "combined count={count} advanced={advanced} input={bytes} written={written:?}"
                );
                assert_eq!(
                    written.bytes, bytes,
                    "combined clusters must retain their order: {written:?}"
                );
                assert_eq!(written.copied, 0);
                let after = run.read_io_counts();
                let reads = after.0 - before.0 + after.1 - before.1;
                assert!(
                    reads <= 8 * run.file_blocks(),
                    "{reads} authenticated reads for {} source blocks",
                    run.file_blocks()
                );
                if !advanced {
                    base.assert_reads(1, 1);
                    assert_eq!(base.requests(), count);
                }
                verify(&records, &resolved, mode, &control);
                drop((resolved, original, run, current, base, store));
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

#[test]
fn combined_occurrence_validation_closes_before_other_committed_reads() {
    for spilled in [false, true] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(256 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let base = crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
            let mut records = records(4, &control);
            for key in [b"a".as_slice(), b"c00001z", b"z"] {
                records.insert(
                    key.to_vec(),
                    (RecordWriteKind::Canonical, b"canonical".to_vec()),
                );
            }
            let original = prepare_records(&records, spilled, &control);
            let resolved = resolve(&original, &base, &base, &Combined, mode, &control).unwrap();
            base.assert_reads(
                if mode == ResolutionMode::Command {
                    5
                } else {
                    1
                },
                1,
            );
            assert_eq!(
                base.requests(),
                if mode == ResolutionMode::Command {
                    7
                } else {
                    4
                }
            );
            verify(&records, &resolved, mode, &control);
            drop((resolved, original, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn combined_occurrence_resolution_preserves_advanced_boundaries() {
    for spilled in [false, true] {
        for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
            let control = StorageReadControl::with_limit(256 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let base = store.snapshot().unwrap();
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
            let current = store.snapshot().unwrap();
            let records = records(4, &control);
            let original = prepare_records(&records, spilled, &control);
            let resolved = resolve(&original, &base, &current, &Combined, mode, &control).unwrap();
            verify(&records, &resolved, mode, &control);
            drop((resolved, original, current, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn combined_occurrence_validation_releases_failed_read_windows() {
    for spilled in [false, true] {
        for failure in ["base", "evaluated", "kind", "cancel", "memory"] {
            let control = StorageReadControl::with_limit(256 << 10);
            let store = MemoryVersionStore::new(control.memory());
            let mut records = records(4, &control);
            if matches!(failure, "base" | "evaluated") {
                records.get_mut(b"c00002".as_slice()).unwrap().1.pop();
            }
            if failure == "kind" {
                records.get_mut(b"c00002".as_slice()).unwrap().0 = RecordWriteKind::OccurrenceCache;
            }
            if failure == "base" {
                let writes: Vec<_> = records
                    .iter()
                    .map(|(key, (_, value))| RecordWrite {
                        key,
                        expected: None,
                        value: Some(value),
                    })
                    .collect();
                store.commit(&writes, &control).unwrap();
                records = self::records(4, &control);
            }
            let base = crate::mvcc::test_support::WindowSnapshot::new(store.snapshot().unwrap());
            let base = if failure == "cancel" {
                base.cancelling_after(1)
            } else {
                base
            };
            let original = prepare_records(&records, spilled, &control);
            let held = (failure == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            let retained = control.memory().used();
            let error = resolve(
                &original,
                &base,
                &base,
                &Combined,
                ResolutionMode::Publication,
                &control,
            )
            .err()
            .expect(failure);
            if failure == "kind" {
                assert!(matches!(
                    error,
                    VersionError::InvalidEncoding("occurrence merge targets a canonical record")
                ));
            }
            assert_eq!(
                control.memory().used(),
                retained,
                "failed resolution retained memory: {failure}"
            );
            if failure == "memory" {
                base.assert_reads(0, 0);
            } else {
                base.assert_reads(1, 0);
                assert_eq!(
                    base.requests(),
                    match failure {
                        "cancel" => 1,
                        "kind" => 2,
                        _ => 3,
                    }
                );
            }
            drop((held, original, base, store));
            assert_eq!(control.memory().used(), 0);
        }
    }
}
