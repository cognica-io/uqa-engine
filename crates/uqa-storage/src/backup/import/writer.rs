//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded physical writes to private record image files, shared by each mapping.

use super::{
    BackupRecordImportMapping, BackupResult, MappedRecordImageImportReader, RecordImageImportUnit,
};
use crate::{
    backup::BackupError,
    mvcc::{BackupCursorKey, CommitSequence},
    read_control::StorageReadControl,
};

/// The writer owns its private transaction until a chunk is flushed, physical import fails, or the destination is dropped. A record payload must be consumed through its verified end.
pub trait BackupRecordImageWriter {
    fn identifier(
        &mut self,
        namespace: &[u8],
        watermark: u64,
        control: &StorageReadControl,
    ) -> BackupResult<()>;
    fn record(
        &mut self,
        key: &[u8],
        revision: CommitSequence,
        value_bytes: usize,
        read: &mut dyn FnMut(&mut [u8]) -> BackupResult<usize>,
        control: &StorageReadControl,
    ) -> BackupResult<()>;
    fn flush(&mut self, control: &StorageReadControl) -> BackupResult<()>;
}

/// Imported rows and the size of the current private write transaction. Large admitted records occupy a transaction on their own.
#[derive(Default)]
pub struct RecordImageImportProgress {
    records: u64,
    identifiers: u64,
    chunk_rows: u16,
    chunk_bytes: u64,
}

impl RecordImageImportProgress {
    pub const fn records(&self) -> u64 {
        self.records
    }
    pub const fn identifiers(&self) -> u64 {
        self.identifiers
    }

    /// Import one already selected record or identifier. Flush before a new unit would take a nonempty chunk past its row or byte allowance.
    pub fn write_current<M: BackupRecordImportMapping>(
        &mut self,
        input: &mut MappedRecordImageImportReader<'_, M>,
        writer: &mut impl BackupRecordImageWriter,
        value_limit: usize,
        resource: &'static str,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let unit = input.current_unit().ok_or(BackupError::Finished)?;
        let bytes = match unit {
            RecordImageImportUnit::Identifier { namespace, .. } => {
                (namespace.len() as u64).saturating_add(8)
            }
            RecordImageImportUnit::Record {
                key, value_bytes, ..
            } => {
                if value_bytes > value_limit as u64 {
                    return Err(BackupError::Limit {
                        resource,
                        required: value_bytes,
                        limit: value_limit as u64,
                    });
                }
                (key.len() as u64)
                    .saturating_add(value_bytes)
                    .saturating_add(8)
            }
        };
        if self.chunk_rows != 0
            && (self.chunk_rows >= 128 || self.chunk_bytes.saturating_add(bytes) > 2 * 1024 * 1024)
        {
            writer.flush(control)?;
            self.chunk_rows = 0;
            self.chunk_bytes = 0;
        }
        match unit {
            RecordImageImportUnit::Identifier {
                namespace,
                watermark,
            } => {
                writer.identifier(namespace, watermark, control)?;
                control.cancellation().check()?;
                self.identifiers += 1;
            }
            RecordImageImportUnit::Record {
                key,
                revision,
                value_bytes,
            } => {
                let mut owned = BackupCursorKey::new(control.memory());
                owned.extend_from_slice(key)?;
                writer.record(
                    &owned,
                    revision,
                    value_bytes as usize,
                    &mut |out| input.read_current(out),
                    control,
                )?;
                control.cancellation().check()?;
                self.records += 1;
            }
        }
        self.chunk_rows += 1;
        self.chunk_bytes = self.chunk_bytes.saturating_add(bytes);
        Ok(())
    }
}
