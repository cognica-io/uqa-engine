//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed Engine generations adapt retained native owners to the shared image stream.

mod catalogs;
mod data;
mod entries;
mod field_analyzers;
mod readers;
mod resources;
mod restore;
mod table_cursor;
mod tables;

use crate::Engine;
use uqa_storage::{
    backup::{
        BackupCut, BackupResult, ImageFormat, ImageProvider, MemoryImageHeader, NativeBackupImage,
        OwnedBackupImage, SnapshotId,
    },
    mvcc::DatabaseId,
    read_control::StorageReadControl,
};

#[cfg(test)]
pub(super) fn capture(
    engine: &Engine,
    control: &StorageReadControl,
) -> BackupResult<OwnedBackupImage> {
    from_snapshot(engine, engine.capture_committed_memory(control)?, control)
}

pub(super) fn from_snapshot(
    engine: &Engine,
    source: crate::state::SnapshotCut,
    control: &StorageReadControl,
) -> BackupResult<OwnedBackupImage> {
    OwnedBackupImage::capture(control, || {
        let identity = engine
            .storage
            .memory_identity
            .as_ref()
            .expect("memory source identity");
        let mut identity = identity.lock();
        let database = if let Some(database) = *identity {
            database
        } else {
            let database = DatabaseId::from_bytes(SnapshotId::new()?.as_bytes());
            *identity = Some(database);
            database
        };
        drop(identity);
        let header = MemoryImageHeader {
            source: database,
            catalog_oid_position: *source.catalog_oids.get()?,
        };
        let cut = BackupCut {
            source: database,
            snapshot: SnapshotId::new()?,
            format: ImageFormat::new(ImageProvider::Memory, 1)?,
            sequence: None,
        };
        NativeBackupImage::new(cut, entries::Entries::new(source, header), control)
    })
}

#[cfg(test)]
mod tests;
