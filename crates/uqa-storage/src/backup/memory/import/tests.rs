//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    backup::*,
    mvcc::{CommitSequence, DatabaseId},
};
use std::io::Cursor;
use uqa_core::RelationIdentity;

mod decode;

#[derive(Clone)]
struct Unit {
    family: u16,
    key: Vec<u8>,
    kind: UnitKind,
    data: Vec<u8>,
}

fn limits() -> BackupLimits {
    BackupLimits::new(4096, 4096).unwrap()
}

fn cut() -> BackupCut {
    BackupCut {
        source: DatabaseId::from_bytes([2; 16]),
        snapshot: SnapshotId::new().unwrap(),
        format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
        sequence: None,
    }
}

fn catalogs() -> Vec<Unit> {
    let mut header = b"UQAMEM01".to_vec();
    header.extend([2; 16]);
    header.push(1);
    header.extend(65_536_u64.to_le_bytes());
    let mut units = vec![Unit {
        family: 1,
        key: Vec::new(),
        kind: UnitKind::Metadata,
        data: header,
    }];
    // Catalog bodies are opaque here; the native catalog owners validate them during import.
    for slot in 1_u16..=24 {
        units.push(Unit {
            family: 2,
            key: slot.to_be_bytes().to_vec(),
            kind: UnitKind::Metadata,
            data: vec![slot as u8],
        });
    }
    units
}

fn table_unit(component: MemoryTableUnit<'_>, kind: UnitKind) -> Unit {
    let control = StorageReadControl::with_limit(4096);
    Unit {
        family: 5,
        key: MemoryImageKey::table(
            &RelationIdentity::new("app", "saved"),
            [3; 16],
            [4; 16],
            component,
            &control,
        )
        .unwrap()
        .into_bytes()
        .to_vec(),
        kind,
        data: b"original component".to_vec(),
    }
}

fn table() -> Vec<Unit> {
    (0..10)
        .map(|slot| table_unit(MemoryTableUnit::Metadata(slot), UnitKind::Metadata))
        .collect()
}

fn publish(
    repository: &LocalRepository,
    cut: BackupCut,
    mut units: Vec<Unit>,
    control: &StorageReadControl,
) -> BackupRoot {
    units.sort_by(|left, right| (left.family, &left.key).cmp(&(right.family, &right.key)));
    let mut operation = repository
        .begin_backup(cut, None, 8 << 20, control)
        .unwrap();
    let mut pack = operation.begin_pack(control).unwrap();
    let ranges = units
        .iter()
        .map(|unit| {
            pack.append(FrameKind::Unit, &mut Cursor::new(&unit.data), control)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let object = pack.finish(control).unwrap();
    let mut inventory = InventoryWriter::new(Vec::new(), cut.format, limits(), control).unwrap();
    for (unit, range) in units.iter().zip(ranges) {
        let entry = InventoryEntry::new(
            UnitAddress {
                namespace: memory_image_namespace(),
                generation: 0,
                family: unit.family,
                key: &unit.key,
            },
            unit.kind,
            unit.data.len() as u64,
            ImageDigest::of_bytes(&unit.data),
            object.span(range).unwrap(),
            limits(),
            control,
        )
        .unwrap();
        inventory.append(&entry).unwrap();
    }
    let (bytes, summary) = inventory.finish().unwrap();
    let inventory = operation
        .write_object(FrameKind::Inventory, &mut Cursor::new(bytes), control)
        .unwrap();
    operation.prepare(inventory, summary, control).unwrap();
    operation.publish(control).unwrap()
}

fn repository(directory: &std::path::Path, control: &StorageReadControl) -> LocalRepository {
    LocalRepository::create(
        directory,
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([8; 16]).unwrap(),
            key: BackupKey::new([42; 32]),
        },
        limits(),
        control,
    )
    .unwrap()
}

fn drain(reader: &mut MemoryImageImportReader<'_>) -> BackupResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 7];
    loop {
        let count = reader.read_current(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn consume(reader: &mut MemoryImageImportReader<'_>) -> BackupResult<usize> {
    let mut count = 0;
    loop {
        match reader.advance(113)? {
            BackupSourceStep::Pending => (),
            BackupSourceStep::Complete => return Ok(count),
            BackupSourceStep::Unit => {
                drain(reader)?;
                count += 1;
            }
        }
    }
}

#[test]
fn memory_import_stream_preserves_units_and_bounds_complete_verification() {
    let directory = tempfile::tempdir().unwrap();
    let setup = StorageReadControl::with_limit(512 << 10);
    let repository = repository(&directory.path().join("repository"), &setup);
    let mut units = catalogs();
    units.push(Unit {
        family: 3,
        key: b"saved\0\0body\0\0".to_vec(),
        kind: UnitKind::Resource,
        data: b"field binding".to_vec(),
    });
    units.push(Unit {
        family: 4,
        key: b"\x01graph\0\0".to_vec(),
        kind: UnitKind::Resource,
        data: b"graph".to_vec(),
    });
    units.extend(table());
    for (component, kind) in [
        (MemoryTableUnit::Document(0), UnitKind::Record),
        (MemoryTableUnit::Text(0), UnitKind::Record),
        (MemoryTableUnit::Vector("vector"), UnitKind::Artifact),
        (
            MemoryTableUnit::ValueIndex {
                name: "index",
                document: None,
            },
            UnitKind::Metadata,
        ),
        (
            MemoryTableUnit::ValueIndex {
                name: "index",
                document: Some(u64::MAX),
            },
            UnitKind::Record,
        ),
    ] {
        units.push(table_unit(component, kind));
    }
    let root = publish(&repository, cut(), units.clone(), &setup);
    let control = StorageReadControl::with_limit(128 << 10);
    let mut reader = MemoryImageImportReader::new(
        repository.open_backup(root.backup(), &control).unwrap(),
        8 << 20,
        &control,
    )
    .unwrap();
    assert_eq!(reader.root(), root);
    assert!(reader.header().is_none());
    let mut count = 0;
    let mut calls = 0;
    loop {
        let before = reader.verified_object_bytes();
        let step = reader.advance(7).unwrap();
        assert!(reader.verified_object_bytes() - before <= 7);
        calls += 1;
        assert!(calls < 100_000);
        match step {
            BackupSourceStep::Complete => break,
            BackupSourceStep::Pending => (),
            BackupSourceStep::Unit => {
                let expected = &units[count + 1].data;
                assert_eq!(
                    reader.current_unit().unwrap().decoded_bytes,
                    expected.len() as u64
                );
                assert_eq!(&drain(&mut reader).unwrap(), expected);
                count += 1;
            }
        }
    }
    assert_eq!(count + 1, units.len());
    assert!(calls > units.len());
    assert!(reader.verified_object_bytes() > 0);
    assert_eq!(reader.header().unwrap().catalog_oid_position, Some(65_536));
    assert!(reader.current_unit().is_none());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert_eq!(reader.advance(0).unwrap(), BackupSourceStep::Complete);
}

#[test]
fn memory_import_stream_rejects_incomplete_catalogs_and_invalid_headers() {
    let directory = tempfile::tempdir().unwrap();
    let setup = StorageReadControl::with_limit(512 << 10);
    let repository = repository(&directory.path().join("repository"), &setup);
    let valid = catalogs();
    let mut variants = vec![
        valid[..1].to_vec(),
        valid[..24].to_vec(),
        valid[1..].to_vec(),
    ];
    let mut gap = valid.clone();
    gap.remove(7);
    variants.push(gap);
    let mut source = valid.clone();
    source[0].data[8] = 99;
    variants.push(source);
    let mut header = valid.clone();
    header[0].data.push(0);
    variants.push(header);
    let mut kind = valid.clone();
    kind[0].kind = UnitKind::Resource;
    variants.push(kind);
    let mut unknown = valid.clone();
    unknown[24].key = 25_u16.to_be_bytes().to_vec();
    variants.push(unknown);
    let mut resource = valid.clone();
    resource.push(Unit {
        family: 4,
        key: b"\x05bad\0\0".to_vec(),
        kind: UnitKind::Resource,
        data: vec![],
    });
    variants.push(resource);
    for units in variants {
        let root = publish(&repository, cut(), units, &setup);
        let control = StorageReadControl::with_limit(128 << 10);
        let mut reader = MemoryImageImportReader::new(
            repository.open_backup(root.backup(), &control).unwrap(),
            8 << 20,
            &control,
        )
        .unwrap();
        assert!(consume(&mut reader).is_err());
        assert!(reader.header().is_none());
        assert!(reader.current_unit().is_none());
        assert!(matches!(reader.advance(1), Err(BackupError::Finished)));
        assert_eq!(control.memory().used(), 0);
        repository.remove_backup(root.backup(), &setup).unwrap();
    }
    let mut sequence = cut();
    sequence.sequence = Some(CommitSequence::from_u64(1));
    let root = publish(&repository, sequence, valid, &setup);
    assert!(matches!(
        MemoryImageImportReader::new(
            repository.open_backup(root.backup(), &setup).unwrap(),
            8 << 20,
            &setup
        ),
        Err(BackupError::Unsupported(_))
    ));
    repository.remove_backup(root.backup(), &setup).unwrap();
}

#[test]
fn memory_import_stream_rejects_partial_table_and_index_membership() {
    let directory = tempfile::tempdir().unwrap();
    let setup = StorageReadControl::with_limit(512 << 10);
    let repository = repository(&directory.path().join("repository"), &setup);
    let mut variants = Vec::new();
    for missing in 0..10 {
        let mut units = table();
        units.remove(missing);
        variants.push(units);
    }
    let mut missing = table();
    missing.push(table_unit(
        MemoryTableUnit::ValueIndex {
            name: "missing",
            document: Some(1),
        },
        UnitKind::Record,
    ));
    variants.push(missing);
    let mut different = table();
    different.push(table_unit(
        MemoryTableUnit::ValueIndex {
            name: "a",
            document: None,
        },
        UnitKind::Metadata,
    ));
    different.push(table_unit(
        MemoryTableUnit::ValueIndex {
            name: "b",
            document: Some(1),
        },
        UnitKind::Record,
    ));
    variants.push(different);
    let mut generation = table();
    let mut other = table_unit(MemoryTableUnit::Metadata(0), UnitKind::Metadata);
    // The fixture has fixed terminated names, then the complete object and generation fields.
    let generation_start = b"app\0\0saved\0\0".len() + 16;
    other.key[generation_start] = 5;
    generation.push(other);
    variants.push(generation);
    let mut kind = table();
    kind[5].kind = UnitKind::Record;
    variants.push(kind);
    for extra in variants {
        let mut units = catalogs();
        units.extend(extra);
        let root = publish(&repository, cut(), units, &setup);
        let control = StorageReadControl::with_limit(128 << 10);
        let mut reader = MemoryImageImportReader::new(
            repository.open_backup(root.backup(), &control).unwrap(),
            8 << 20,
            &control,
        )
        .unwrap();
        assert!(consume(&mut reader).is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(reader.advance(1), Err(BackupError::Finished)));
    }
}

#[test]
fn memory_import_stream_cancellation_and_unread_units_release_admission() {
    let directory = tempfile::tempdir().unwrap();
    let setup = StorageReadControl::with_limit(512 << 10);
    let repository = repository(&directory.path().join("repository"), &setup);
    for cancel_after in [0, 2, 24] {
        let root = publish(&repository, cut(), catalogs(), &setup);
        let control = StorageReadControl::with_limit(128 << 10);
        let mut reader = MemoryImageImportReader::new(
            repository.open_backup(root.backup(), &control).unwrap(),
            8 << 20,
            &control,
        )
        .unwrap();
        let mut count = 0;
        while count < cancel_after {
            if reader.advance(1).unwrap() == BackupSourceStep::Unit {
                drain(&mut reader).unwrap();
                count += 1;
            }
        }
        control.cancellation().cancel();
        assert!(reader.advance(1).is_err());
        assert!(reader.header().is_none());
        assert_eq!(control.memory().used(), 0);
        repository.remove_backup(root.backup(), &setup).unwrap();
    }
    let root = publish(&repository, cut(), catalogs(), &setup);
    let control = StorageReadControl::with_limit(128 << 10);
    let mut reader = MemoryImageImportReader::new(
        repository.open_backup(root.backup(), &control).unwrap(),
        8 << 20,
        &control,
    )
    .unwrap();
    while reader.advance(1).unwrap() != BackupSourceStep::Unit {}
    assert!(reader.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(
        reader.read_current(&mut [0]),
        Err(BackupError::Finished)
    ));
    repository.remove_backup(root.backup(), &setup).unwrap();
}
