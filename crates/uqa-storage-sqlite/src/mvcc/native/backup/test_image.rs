//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal archive headers distinguish mapping admission from row materialization.

use super::*;
use rusqlite::types::ValueRef;
use uqa_storage::backup::BackupRoot;
use uqa_storage::{
    backup::{
        BackupImageSource, BackupLimits, BackupSourceStatistics, BackupSourceUnit, LocalRepository,
        Protection, SnapshotId, UnitAddress, UnitKind,
    },
    mvcc::CommitSequence,
};

#[derive(Clone)]
pub(super) struct Unit {
    pub(super) family: u16,
    pub(super) key: Vec<u8>,
    pub(super) bytes: Vec<u8>,
    pub(super) kind: UnitKind,
}
#[derive(Clone)]
pub(super) struct Image {
    pub(super) cut: BackupCut,
    pub(super) units: Vec<Unit>,
    pub(super) current: Option<usize>,
    pub(super) offset: usize,
}
impl Image {
    pub(super) fn capture(mut source: impl BackupImageSource) -> Self {
        let mut image = Self {
            cut: source.cut(),
            units: Vec::new(),
            current: None,
            offset: 0,
        };
        loop {
            match source.advance(16 * 1024).unwrap() {
                BackupSourceStep::Complete => return image,
                BackupSourceStep::Pending => continue,
                BackupSourceStep::Unit => (),
            }
            let unit = source.current_unit().unwrap();
            let mut owned = Unit {
                family: unit.address.family,
                key: unit.address.key.to_vec(),
                bytes: Vec::new(),
                kind: unit.kind,
            };
            let mut output = [0; 4096];
            loop {
                let count = source.read_current(&mut output).unwrap();
                if count == 0 {
                    break;
                }
                owned.bytes.extend_from_slice(&output[..count]);
            }
            image.units.push(owned);
        }
    }
    pub(super) fn new(payload: Vec<u8>) -> Self {
        let control = StorageReadControl::with_limit(1 << 20);
        let key = NativeRecordIdentity::new(
            NativeRecordFamily::Metadata,
            NativeRecordOwner::Database(DatabaseId::from_bytes([11; 16])),
        )
        .unwrap()
        .encode_key(&[ValueRef::Text(b"literal")], &control)
        .unwrap()
        .to_vec();
        let mut header = b"UQANVI01".to_vec();
        header.extend_from_slice(&[5; 16]);
        header.extend_from_slice(&9u64.to_be_bytes());
        header.extend_from_slice(&[11; 16]);
        header.extend_from_slice(&17u32.to_be_bytes());
        header.extend_from_slice(&63u16.to_be_bytes());
        header.extend_from_slice(&49u16.to_be_bytes());
        let mut bytes = 7u64.to_be_bytes().to_vec();
        bytes.extend(payload);
        Self {
            cut: BackupCut {
                source: DatabaseId::from_bytes([5; 16]),
                snapshot: SnapshotId::from_bytes([6; 16]).unwrap(),
                format: ImageFormat::new(ImageProvider::SQLiteNative, 1).unwrap(),
                sequence: Some(CommitSequence::from_u64(9)),
            },
            units: vec![
                Unit {
                    family: 1,
                    key: vec![],
                    bytes: header,
                    kind: UnitKind::Metadata,
                },
                Unit {
                    family: 2,
                    key: b"allocator".to_vec(),
                    bytes: 77u64.to_be_bytes().to_vec(),
                    kind: UnitKind::Metadata,
                },
                Unit {
                    family: 3,
                    key,
                    bytes,
                    kind: UnitKind::Record,
                },
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
                namespace: NamespaceId::from_bytes(*b"UQA-NATIVE-IMG01").unwrap(),
                generation: 0,
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
pub(super) fn repository(
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
        let mut backup = repository
            .begin_backup(image.cut(), None, 8 << 20, control)
            .unwrap();
        backup.prepare_image(image, 2 << 20, control).unwrap();
        backup.publish(control).unwrap()
    };
    (repository, root)
}
