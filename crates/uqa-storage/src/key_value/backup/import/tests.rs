//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent image envelopes exercise the common importer without a provider implementation.

use super::*;
use crate::{
    backup::*,
    mvcc::{CommitSequence, DatabaseId, RECLAMATION_DOMAIN_PREFIX, RECLAMATION_EPOCH_NAMESPACE},
};

struct Unit {
    namespace: [u8; 16],
    generation: u64,
    family: u16,
    key: Vec<u8>,
    kind: UnitKind,
    bytes: Vec<u8>,
}

struct Image {
    cut: BackupCut,
    units: Vec<Unit>,
    current: Option<usize>,
    offset: usize,
}

impl Image {
    fn new(provider: ImageProvider) -> Self {
        let cut = BackupCut {
            source: DatabaseId::from_bytes([5; 16]),
            snapshot: SnapshotId::from_bytes([9; 16]).unwrap(),
            format: ImageFormat::new(provider, 1).unwrap(),
            sequence: Some(CommitSequence::from_u64(9)),
        };
        let mut header = b"UQAKVI01".to_vec();
        header.extend_from_slice(&[5; 16]);
        header.extend_from_slice(&9_u64.to_be_bytes());
        let unit = |family, key: &[u8], kind, bytes| Unit {
            namespace: *b"UQA-KV-IMAGE-001",
            generation: 0,
            family,
            key: key.to_vec(),
            kind,
            bytes,
        };
        Self {
            cut,
            units: vec![
                unit(1, b"", UnitKind::Metadata, header),
                unit(
                    2,
                    b"allocator",
                    UnitKind::Metadata,
                    77_u64.to_be_bytes().to_vec(),
                ),
                unit(3, b"", UnitKind::Record, 9_u64.to_be_bytes().to_vec()),
            ],
            current: None,
            offset: 0,
        }
    }
}

impl BackupImageSource for Image {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn advance(&mut self, _: usize) -> BackupResult<BackupSourceStep> {
        let index = self.current.map_or(0, |i| i + 1);
        self.current = Some(index);
        self.offset = 0;
        Ok(if index == self.units.len() {
            BackupSourceStep::Complete
        } else {
            BackupSourceStep::Unit
        })
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        let unit = self.units.get(self.current?)?;
        Some(BackupSourceUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes(unit.namespace).unwrap(),
                generation: unit.generation,
                family: unit.family,
                key: &unit.key,
            },
            kind: unit.kind,
            decoded_bytes: unit.bytes.len() as u64,
        })
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let unit = &self.units[self.current.unwrap()];
        let n = output.len().min(unit.bytes.len() - self.offset);
        output[..n].copy_from_slice(&unit.bytes[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        BackupSourceStatistics::default()
    }
}

fn repository(
    path: &std::path::Path,
    image: Image,
    control: &StorageReadControl,
) -> (LocalRepository, BackupRoot) {
    let repository = LocalRepository::create(
        path,
        Protection::Plaintext,
        BackupLimits::new(512, 512).unwrap(),
        control,
    )
    .unwrap();
    let root = {
        let mut operation = repository
            .begin_backup(image.cut(), None, 1 << 20, control)
            .unwrap();
        operation.prepare_image(image, 1 << 20, control).unwrap();
        operation.publish(control).unwrap()
    };
    (repository, root)
}

#[test]
fn key_value_backup_import_accepts_literal_header_allocators_and_empty_records_for_both_mappings() {
    for provider in [ImageProvider::Redb, ImageProvider::SQLiteKV] {
        let temporary = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let (repository, root) = repository(
            &temporary.path().join("backup"),
            Image::new(provider),
            &control,
        );
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let mut input =
            KeyValueImageImportReader::new(&lease, provider, 1 << 20, &control).unwrap();
        let mut units = 0;
        loop {
            match input.advance(1).unwrap() {
                KeyValueImageStep::Pending => continue,
                KeyValueImageStep::Complete => break,
                KeyValueImageStep::Unit => (),
            }
            match input.current_unit().unwrap() {
                KeyValueImportUnit::Identifier {
                    namespace,
                    watermark,
                } => {
                    assert_eq!(namespace, b"allocator");
                    assert_eq!(watermark, 77);
                }
                KeyValueImportUnit::Record {
                    key,
                    revision,
                    value_bytes,
                } => {
                    assert_eq!(key, b"");
                    assert_eq!(revision.as_u64(), 9);
                    assert_eq!(value_bytes, 0);
                    assert_eq!(input.read_current(&mut [0]).unwrap(), 0);
                }
            }
            units += 1;
        }
        assert_eq!(units, 2);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= 64 << 10);
        control.cancellation().cancel();
        assert_eq!(input.advance(0).unwrap(), KeyValueImageStep::Complete);
        assert!(matches!(
            input.read_current(&mut [0]),
            Err(BackupError::Finished)
        ));
        assert_eq!(input.advance(0).unwrap(), KeyValueImageStep::Complete);
    }
}

#[test]
fn key_value_backup_import_rejects_unknown_image_versions_and_absent_sequences() {
    for fault in 0..2 {
        let temporary = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let mut image = Image::new(ImageProvider::Redb);
        if fault == 0 {
            image.cut.format = ImageFormat::new(ImageProvider::Redb, 2).unwrap();
        } else {
            image.cut.sequence = None;
        }
        let (repository, root) = repository(&temporary.path().join("backup"), image, &control);
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        assert!(matches!(
            KeyValueImageImportReader::new(&lease, ImageProvider::Redb, 1 << 20, &control),
            Err(BackupError::Unsupported(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn key_value_backup_import_rejects_authenticated_but_invalid_mapping_envelopes() {
    for provider in [ImageProvider::Redb, ImageProvider::SQLiteKV] {
        for fault in 0..22 {
            let temporary = tempfile::tempdir().unwrap();
            let control = StorageReadControl::with_limit(64 << 10);
            let mut image = Image::new(provider);
            match fault {
                0 => {
                    image.units.remove(0);
                }
                1 => image.units[0].bytes[0] ^= 1,
                2 => image.units[0].bytes[8] ^= 1,
                3 => image.units[0].bytes[31] ^= 1,
                4 => {
                    for unit in &mut image.units {
                        unit.namespace = [2; 16];
                    }
                }
                5 => {
                    for unit in &mut image.units {
                        unit.generation = 1;
                    }
                }
                6 => image.units[0].key = b"unexpected".to_vec(),
                7 => image.units[0].kind = UnitKind::Record,
                8 => {
                    image.units[0].bytes.pop();
                }
                9 => {
                    image.units[1].family = 1;
                }
                10 => image.units[1].key.clear(),
                11 => image.units[1].key = RECLAMATION_EPOCH_NAMESPACE.to_vec(),
                12 => image.units[1].key = RECLAMATION_DOMAIN_PREFIX.to_vec(),
                13 => {
                    image.units[1].bytes.pop();
                }
                14 => image.units[1].kind = UnitKind::Record,
                15 => image.units[2].bytes = 0_u64.to_be_bytes().to_vec(),
                16 => image.units[2].bytes = 10_u64.to_be_bytes().to_vec(),
                17 => image.units[2].kind = UnitKind::Metadata,
                18 => {
                    image.units[2].bytes.pop();
                }
                19 => {
                    image.units[2].key = crate::key_value::KeyValueNotificationRecords
                        .key(&control)
                        .unwrap()
                        .to_vec();
                }
                20 => image.units[2].family = 4,
                _ => image.units.clear(),
            }
            let (repository, root) = repository(&temporary.path().join("backup"), image, &control);
            let lease = repository.open_backup(root.backup(), &control).unwrap();
            let mut input =
                KeyValueImageImportReader::new(&lease, provider, 1 << 20, &control).unwrap();
            let failure = loop {
                match input.advance(1024) {
                    Ok(KeyValueImageStep::Complete) => panic!("fault {fault} accepted"),
                    Ok(KeyValueImageStep::Pending) => (),
                    Ok(KeyValueImageStep::Unit) => {
                        if matches!(
                            input.current_unit(),
                            Some(KeyValueImportUnit::Record { .. })
                        ) {
                            while input.read_current(&mut [0; 64]).unwrap() > 0 {}
                        }
                    }
                    Err(error) => break error,
                }
            };
            assert!(
                matches!(failure, BackupError::Invalid(_)),
                "fault {fault}: {failure}"
            );
            assert_eq!(
                control.memory().used(),
                0,
                "fault {fault} retained resources"
            );
            assert!(matches!(input.advance(1024), Err(BackupError::Finished)));
        }
    }
}

#[test]
fn key_value_backup_import_rejects_incompatible_roots_and_unconsumed_record_values() {
    let temporary = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let mut image = Image::new(ImageProvider::Redb);
    image.units[2].bytes.extend_from_slice(b"raw value");
    let (repository, root) = repository(&temporary.path().join("backup"), image, &control);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    for provider in [
        ImageProvider::SQLiteKV,
        ImageProvider::SQLiteNative,
        ImageProvider::Memory,
    ] {
        assert!(matches!(
            KeyValueImageImportReader::new(&lease, provider, 1 << 20, &control),
            Err(BackupError::Unsupported(_))
        ));
    }
    for fault in 0..3 {
        let mut input =
            KeyValueImageImportReader::new(&lease, ImageProvider::Redb, 1 << 20, &control).unwrap();
        while !matches!(
            input.current_unit(),
            Some(KeyValueImportUnit::Record { .. })
        ) {
            input.advance(1024).unwrap();
        }
        if fault == 0 {
            input.read_current(&mut [0]).unwrap();
        }
        let result = match fault {
            0 | 1 => input.advance(1024).map(|_| ()),
            _ => input.read_current(&mut []).map(|_| ()),
        };
        assert!(result.is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(input.advance(1024), Err(BackupError::Finished)));
    }
}
