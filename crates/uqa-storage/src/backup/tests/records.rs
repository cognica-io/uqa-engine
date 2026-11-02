//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor lifecycle and exact input bytes over an independent committed-record fixture.

use super::*;
use crate::mvcc::{
    retain_backup_snapshot, BackupCursorKey, BackupRecordPositionVisitor, CommittedBackupSnapshot,
    CommittedRecordSnapshot, IdentifierWatermarkVisitor, MemoryRecordSnapshot, MemoryVersionStore,
    MetadataEpoch, RecordWrite, VersionError, VersionResult,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::memory::BudgetedVec;

struct Source {
    records: MemoryRecordSnapshot,
    failure: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

const KEYS: [&[u8]; 6] = [b"", b"dead", b"future", b"large", b"zero", b"zz_future"];
const WATERMARKS: [(&[u8], Option<u64>); 2] = [(b"ids", Some(42)), (b"later", None)];

fn key(bytes: &[u8], control: &StorageReadControl) -> VersionResult<BackupCursorKey> {
    let mut result = BackupCursorKey::new(control.memory());
    result.extend_from_slice(bytes)?;
    Ok(result)
}

impl Drop for Source {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl CommittedBackupSnapshot for Source {
    fn records(&self) -> &dyn CommittedRecordSnapshot {
        &self.records
    }
    fn database_id(&self) -> DatabaseId {
        DatabaseId::from_bytes([7; 16])
    }
    fn metadata_epoch(&self) -> MetadataEpoch {
        MetadataEpoch::INITIAL
    }
    fn record_key_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BackupCursorKey>> {
        Ok(Some(key(KEYS.last().unwrap(), control)?))
    }
    fn identifier_namespace_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BackupCursorKey>> {
        Ok(Some(key(WATERMARKS.last().unwrap().0, control)?))
    }
    fn visit_identifier_watermarks(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        _: &StorageReadControl,
        visit: &mut IdentifierWatermarkVisitor<'_>,
    ) -> VersionResult<()> {
        for (key, value) in WATERMARKS
            .iter()
            .filter(|(key, _)| after.is_none_or(|after| *key > after))
            .take(limit)
        {
            if !visit(key, *value)? {
                break;
            }
        }
        Ok(())
    }
    fn visit_record_positions(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut BackupRecordPositionVisitor<'_>,
    ) -> VersionResult<()> {
        match self.failure.load(Ordering::SeqCst) {
            1 => {
                return Err(VersionError::InvalidEncoding(
                    "injected image source failure",
                ))
            }
            2 => {
                control.cancellation().cancel();
                control.check()?;
            }
            3 => {
                control
                    .memory()
                    .reserve(usize::MAX)
                    .map_err(crate::StorageBackendError::from)?;
            }
            _ => (),
        }
        for key in KEYS
            .iter()
            .filter(|key| after.is_none_or(|after| **key > after))
            .take(limit)
        {
            if !visit(key, self.records.metadata(key, control)?)? {
                break;
            }
        }
        Ok(())
    }
}

struct Fixture {
    store: MemoryVersionStore,
    source: Option<Arc<dyn CommittedBackupSnapshot>>,
    setup: StorageReadControl,
    control: StorageReadControl,
    large: Vec<u8>,
    revision: CommitSequence,
    dropped: Arc<AtomicUsize>,
    failure: Arc<AtomicUsize>,
}

impl Fixture {
    fn new() -> Self {
        let setup = StorageReadControl::with_limit(32 << 20);
        let store = MemoryVersionStore::new(setup.memory());
        let large: Vec<_> = (0..(1 << 20) + 17).map(|i| (i % 251) as u8).collect();
        let revision = store
            .commit(
                &[
                    RecordWrite {
                        key: b"",
                        expected: None,
                        value: Some(b"initial"),
                    },
                    RecordWrite {
                        key: b"dead",
                        expected: None,
                        value: None,
                    },
                    RecordWrite {
                        key: b"large",
                        expected: None,
                        value: Some(&large),
                    },
                    RecordWrite {
                        key: b"zero",
                        expected: None,
                        value: Some(b""),
                    },
                ],
                &setup,
            )
            .unwrap();
        let control = StorageReadControl::with_limit(4096);
        let dropped = Arc::new(AtomicUsize::new(0));
        let failure = Arc::new(AtomicUsize::new(0));
        let source = retain_backup_snapshot(
            Source {
                records: store.snapshot().unwrap(),
                dropped: dropped.clone(),
                failure: failure.clone(),
            },
            &control,
        )
        .unwrap();
        Self {
            store,
            source: Some(source),
            setup,
            control,
            large,
            revision,
            dropped,
            failure,
        }
    }
    fn cursor(&mut self) -> RecordImageCursor {
        RecordImageCursor::new(self.source.take().unwrap(), &self.control).unwrap()
    }
}

#[test]
fn record_image_cursor_streams_exact_units_under_a_fixed_allowance() {
    let mut fixture = Fixture::new();
    let mut cursor = fixture.cursor();
    assert_eq!(cursor.source_id(), DatabaseId::from_bytes([7; 16]));
    assert_eq!(cursor.sequence(), fixture.revision);
    let mut output = BudgetedVec::new(fixture.control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut units = Vec::new();
    loop {
        match cursor.advance(1).unwrap() {
            RecordImageStep::Complete => break,
            RecordImageStep::Pending => continue,
            RecordImageStep::Unit => (),
        }
        let unit = cursor.current_unit().unwrap();
        let family = unit.family;
        let name = unit.key.to_vec();
        let watermark = 42_u64.to_be_bytes();
        let expected: &[u8] = match (family, unit.key) {
            (RecordImageFamily::IdentifierWatermark, b"ids") => &watermark,
            (RecordImageFamily::Record, b"") => b"initial",
            (RecordImageFamily::Record, b"large") => &fixture.large,
            (RecordImageFamily::Record, b"zero") => b"",
            _ => panic!("unexpected image unit"),
        };
        assert_eq!(unit.decoded_bytes, expected.len());
        assert_eq!(
            unit.revision,
            (family == RecordImageFamily::Record).then_some(fixture.revision)
        );
        units.push((family, name));
        let mut offset = 0;
        while offset < expected.len() {
            let copied = cursor.read_current(&mut output).unwrap();
            assert_eq!(&output[..copied], &expected[offset..offset + copied]);
            offset += copied;
            if family == RecordImageFamily::Record && expected.len() > 1024 && offset == 1024 {
                fixture
                    .store
                    .commit(
                        &[RecordWrite {
                            key: b"large",
                            expected: Some(fixture.revision),
                            value: Some(b"replacement"),
                        }],
                        &fixture.setup,
                    )
                    .unwrap();
            }
        }
    }
    assert_eq!(
        units,
        vec![
            (RecordImageFamily::IdentifierWatermark, b"ids".to_vec()),
            (RecordImageFamily::Record, b"".to_vec()),
            (RecordImageFamily::Record, b"large".to_vec()),
            (RecordImageFamily::Record, b"zero".to_vec())
        ]
    );
    assert_eq!(cursor.visited_positions(), 8);
    assert_eq!(cursor.copied_bytes(), 8 + 7 + fixture.large.len() as u64);
    assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.control.memory().used(), output.capacity());
    drop(output);
    assert_eq!(fixture.control.memory().used(), 0);
    fixture.control.cancellation().cancel();
    assert_eq!(cursor.advance(0).unwrap(), RecordImageStep::Complete);
    assert!(matches!(
        cursor.read_current(&mut []),
        Err(BackupError::Finished)
    ));
    assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Complete);
}

#[test]
fn record_image_cursor_releases_its_cut_on_failure_or_unconsumed_advance() {
    let mut fixture = Fixture::new();
    let mut cursor = fixture.cursor();
    assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Unit);
    assert_eq!(cursor.read_current(&mut [0; 2]).unwrap(), 2);
    assert!(cursor.advance(1).is_err());
    assert!(cursor.current_unit().is_none());
    assert_eq!(fixture.control.memory().used(), 0);
    assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);
    fixture.control.cancellation().cancel();
    assert!(matches!(cursor.advance(1), Err(BackupError::Finished)));

    let mut fixture = Fixture::new();
    let mut cursor = fixture.cursor();
    assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Unit);
    cursor.read_current(&mut [0; 8]).unwrap();
    fixture.failure.store(1, Ordering::SeqCst);
    while let Ok(step) = cursor.advance(1) {
        assert_eq!(step, RecordImageStep::Pending);
    }
    assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.control.memory().used(), 0);
    assert!(matches!(
        cursor.read_current(&mut []),
        Err(BackupError::Finished)
    ));
}

#[test]
fn record_image_cursor_cancellation_and_failed_creation_release_retention() {
    let mut fixture = Fixture::new();
    fixture.control.cancellation().cancel();
    assert!(matches!(
        RecordImageCursor::new(fixture.source.take().unwrap(), &fixture.control),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(fixture.control.memory().used(), 0);
    assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);

    let mut fixture = Fixture::new();
    let mut cursor = fixture.cursor();
    cursor.advance(1).unwrap();
    fixture.control.cancellation().cancel();
    assert!(matches!(
        cursor.read_current(&mut [0; 8]),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(fixture.control.memory().used(), 0);
    assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn record_image_cursor_keeps_resource_errors_from_provider_read_boundaries() {
    for failure in [2, 3] {
        let mut fixture = Fixture::new();
        let mut cursor = fixture.cursor();
        assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Unit);
        cursor.read_current(&mut [0; 8]).unwrap();
        fixture.failure.store(failure, Ordering::SeqCst);
        let error = loop {
            match cursor.advance(1) {
                Ok(step) => assert_eq!(step, RecordImageStep::Pending),
                Err(error) => break error,
            }
        };
        match failure {
            2 => assert!(matches!(error, BackupError::Cancelled(_))),
            3 => assert!(matches!(error, BackupError::Memory(_))),
            _ => unreachable!(),
        }
        assert!(cursor.current_unit().is_none());
        assert_eq!(fixture.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.control.memory().used(), 0);
        assert!(matches!(cursor.advance(1), Err(BackupError::Finished)));
    }
}
