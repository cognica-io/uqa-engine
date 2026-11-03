//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Cursor;

use crate::mvcc::{CommitSequence, DatabaseId};
use crate::read_control::StorageReadControl;

use super::*;

mod frames;
mod inventories;
mod records;
mod roots;

fn limits() -> BackupLimits {
    BackupLimits::new(4096, 65536).unwrap()
}
fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1024 * 1024)
}
fn format() -> ImageFormat {
    ImageFormat::new(ImageProvider::SQLiteNative, 1).unwrap()
}

fn context() -> FrameContext {
    FrameContext {
        repository: RepositoryId::from_bytes([1; 16]).unwrap(),
        object: ObjectId::from_bytes([2; 16]).unwrap(),
        offset: 64,
        kind: FrameKind::Unit,
    }
}

fn protection() -> Protection {
    Protection::Encrypted {
        domain: ProtectionId::from_bytes([9; 16]).unwrap(),
        key: BackupKey::new([42; 32]),
    }
}

fn span() -> ObjectSpan {
    ObjectSpan::new(
        context().object,
        ImageDigest::from_bytes([4; 32]),
        1024,
        128,
        256,
        1,
    )
    .unwrap()
}

fn address(key: &[u8]) -> UnitAddress<'_> {
    UnitAddress {
        namespace: NamespaceId::from_bytes([3; 16]).unwrap(),
        generation: 7,
        family: 2,
        key,
    }
}

fn entry(key: &[u8], control: &StorageReadControl) -> InventoryEntry {
    InventoryEntry::new(
        address(key),
        UnitKind::Record,
        3,
        ImageDigest::of_bytes(b"abc"),
        span(),
        limits(),
        control,
    )
    .unwrap()
}

fn inventory() -> (Vec<u8>, InventorySummary) {
    let control = control();
    let mut writer = InventoryWriter::new(Vec::new(), format(), limits(), &control).unwrap();
    writer.append(&entry(b"alpha", &control)).unwrap();
    writer.finish().unwrap()
}

fn root() -> BackupRoot {
    BackupRoot::new(
        context().repository,
        BackupId::from_bytes([5; 16]).unwrap(),
        None,
        BackupCut {
            source: DatabaseId::from_bytes([6; 16]),
            snapshot: SnapshotId::from_bytes([7; 16]).unwrap(),
            format: format(),
            sequence: Some(CommitSequence::from_u64(11)),
        },
        span(),
        inventory().1,
    )
    .unwrap()
}

fn hex_fixture(text: &str) -> Vec<u8> {
    let text: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
