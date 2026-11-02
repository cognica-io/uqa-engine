//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned Key/Value images preserve owner encodings and project out delivery and build state.

use std::sync::Arc;

use crate::{
    backup::{
        BackupCut, BackupError, BackupResult, ImageFormat, ImageProvider, NamespaceId,
        RecordImageCursor, RecordImageFamily, RecordImageStep, SnapshotId, UnitAddress, UnitKind,
    },
    mvcc::{BackupCursorKey, CommittedBackupSnapshot, NotificationRecordLayout},
    read_control::StorageReadControl,
};

const NAMESPACE: [u8; 16] = *b"UQA-KV-IMAGE-001";
const HEADER_BYTES: usize = 32;

/// One complete provider image unit. Record payloads begin with their committed revision as eight big-endian bytes; allocator payloads are eight big-endian bytes. The header and all owner keys are versioned by the image format.
pub struct KeyValueImageUnit<'a> {
    pub address: UnitAddress<'a>,
    pub kind: UnitKind,
    pub decoded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyValueImageStep {
    Unit,
    Pending,
    Complete,
}

enum Stage {
    Ready,
    Header(usize),
    Scan,
    Unit(usize),
    Complete,
    Failed,
}

/// Online Key/Value mapping exporter over one already captured joint cut. This owner interprets the mapping's notification and `DiskANN` lifecycle records, while preserving catalog, evaluated data/index/resource bytes and data namespaces verbatim. Import and public Engine capability qualification are separate provider responsibilities.
pub struct KeyValueBackupImage {
    cut: BackupCut,
    source: Option<Arc<dyn CommittedBackupSnapshot>>,
    records: Option<RecordImageCursor>,
    notification_key: BackupCursorKey,
    control: StorageReadControl,
    stage: Stage,
    header: [u8; HEADER_BYTES],
    positions: u64,
    copied_bytes: u64,
    metadata_read_bytes: u64,
    excluded_units: u64,
}

impl KeyValueBackupImage {
    /// The provider must supply its complete Key/Value mapping cut. Native `SQLite` and the actual memory Engine have different owners and cannot use this constructor to claim image support.
    pub fn new(
        source: Arc<dyn CommittedBackupSnapshot>,
        provider: ImageProvider,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        if !matches!(provider, ImageProvider::SQLiteKV | ImageProvider::Redb) {
            return Err(BackupError::Unsupported("Key/Value image provider"));
        }
        control.cancellation().check()?;
        let cut = BackupCut {
            source: source.database_id(),
            snapshot: SnapshotId::new()?,
            format: ImageFormat::new(provider, 1)?,
            sequence: Some(source.records().sequence()),
        };
        let mut header = [0; HEADER_BYTES];
        header[..8].copy_from_slice(b"UQAKVI01");
        header[8..24].copy_from_slice(&cut.source.as_bytes());
        header[24..].copy_from_slice(&source.records().sequence().as_u64().to_be_bytes());
        let notification_key = super::KeyValueNotificationRecords.key(control)?;
        let records = RecordImageCursor::new(source.clone(), control)?;
        Ok(Self {
            cut,
            source: Some(source),
            records: Some(records),
            notification_key,
            control: control.clone(),
            stage: Stage::Ready,
            header,
            positions: 0,
            copied_bytes: 0,
            metadata_read_bytes: 0,
            excluded_units: 0,
        })
    }

    pub const fn cut(&self) -> BackupCut {
        self.cut
    }

    /// Actual scan positions, including excluded entries; this is not page I/O.
    pub const fn visited_positions(&self) -> u64 {
        self.positions
    }

    /// Raw source record/allocator bytes copied into caller output. Generated headers and revision prefixes are excluded.
    pub const fn copied_source_bytes(&self) -> u64 {
        self.copied_bytes
    }

    /// Generation-state bytes read for image membership, separately from streamed payloads. Opaque provider cache and page activity are not measured here.
    pub const fn metadata_read_bytes(&self) -> u64 {
        self.metadata_read_bytes
    }

    pub const fn excluded_units(&self) -> u64 {
        self.excluded_units
    }

    pub fn current_unit(&self) -> Option<KeyValueImageUnit<'_>> {
        let (family, key, kind, decoded_bytes) = match self.stage {
            Stage::Header(_) => (1, &[][..], UnitKind::Metadata, HEADER_BYTES as u64),
            Stage::Unit(_) => {
                let unit = self.records.as_ref()?.current_unit()?;
                match unit.family {
                    RecordImageFamily::IdentifierWatermark => {
                        (2, unit.key, UnitKind::Metadata, unit.decoded_bytes as u64)
                    }
                    RecordImageFamily::Record => {
                        (3, unit.key, UnitKind::Record, unit.decoded_bytes as u64 + 8)
                    }
                }
            }
            _ => return None,
        };
        Some(KeyValueImageUnit {
            address: UnitAddress {
                namespace: NamespaceId::from_bytes(NAMESPACE).expect("fixed nonzero namespace"),
                generation: 0,
                family,
                key,
            },
            kind,
            decoded_bytes,
        })
    }

    /// Inspect at most `positions` raw positions and at most one generation-state record. An excluded unit returns Pending without reading its payload. Every physical read closes before return, including when a selected unit still has unread bytes.
    pub fn advance(&mut self, positions: usize) -> BackupResult<KeyValueImageStep> {
        let result = self.advance_inner(positions);
        self.finish_call(&result);
        result
    }

    fn advance_inner(&mut self, positions: usize) -> BackupResult<KeyValueImageStep> {
        match self.stage {
            Stage::Complete => return Ok(KeyValueImageStep::Complete),
            Stage::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        self.control.cancellation().check()?;
        if positions == 0 {
            return Err(BackupError::Invalid("zero Key/Value image scan window"));
        }
        match self.stage {
            Stage::Ready => {
                self.stage = Stage::Header(0);
                return Ok(KeyValueImageStep::Unit);
            }
            Stage::Header(offset) if offset != HEADER_BYTES => {
                return Err(BackupError::Invalid(
                    "Key/Value image header is not consumed",
                ));
            }
            Stage::Unit(offset)
                if offset < 8
                    && self
                        .records
                        .as_ref()
                        .unwrap()
                        .current_unit()
                        .unwrap()
                        .family
                        == RecordImageFamily::Record =>
            {
                return Err(BackupError::Invalid(
                    "Key/Value image revision is not consumed",
                ));
            }
            _ => (),
        }
        let records = self.records.as_mut().expect("active image owns cursor");
        match records.advance(positions)? {
            RecordImageStep::Complete => {
                self.release(Stage::Complete);
                Ok(KeyValueImageStep::Complete)
            }
            RecordImageStep::Pending => {
                self.stage = Stage::Scan;
                Ok(KeyValueImageStep::Pending)
            }
            RecordImageStep::Unit => {
                let unit = records.current_unit().expect("selected raw unit");
                let (include, bytes) = match unit.family {
                    RecordImageFamily::IdentifierWatermark => (true, 0),
                    RecordImageFamily::Record if unit.key == &*self.notification_key => (false, 0),
                    RecordImageFamily::Record => super::diskann::backup::include(
                        self.source
                            .as_ref()
                            .expect("active image owns cut")
                            .records(),
                        unit.key,
                        &self.control,
                    )?,
                };
                self.metadata_read_bytes = self
                    .metadata_read_bytes
                    .checked_add(bytes as u64)
                    .ok_or(BackupError::Invalid("image counter overflow"))?;
                if !include {
                    records.skip_current()?;
                    self.excluded_units = self
                        .excluded_units
                        .checked_add(1)
                        .ok_or(BackupError::Invalid("image counter overflow"))?;
                    self.stage = Stage::Scan;
                    return Ok(KeyValueImageStep::Pending);
                }
                if unit.family == RecordImageFamily::Record {
                    (unit.decoded_bytes as u64)
                        .checked_add(8)
                        .ok_or(BackupError::Invalid(
                            "Key/Value image record length overflow",
                        ))?;
                }
                self.stage = Stage::Unit(0);
                Ok(KeyValueImageStep::Unit)
            }
        }
    }

    /// Copy the current unit into a caller-owned buffer. Errors make the image terminal and release the cut; discard the output of a failed call.
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = self.read_inner(output);
        self.finish_call(&result);
        result
    }

    fn read_inner(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        if !matches!(self.stage, Stage::Header(_) | Stage::Unit(_)) {
            return Err(BackupError::Finished);
        }
        self.control.cancellation().check()?;
        match self.stage {
            Stage::Header(ref mut offset) => {
                let size = output.len().min(HEADER_BYTES - *offset);
                output[..size].copy_from_slice(&self.header[*offset..*offset + size]);
                *offset += size;
                Ok(size)
            }
            Stage::Unit(ref mut offset) => {
                let records = self.records.as_mut().expect("active image owns cursor");
                let unit = records.current_unit().ok_or(BackupError::Finished)?;
                let mut copied = 0;
                if let Some(revision) = unit.revision {
                    copied = output.len().min(8 - *offset);
                    output[..copied].copy_from_slice(
                        &revision.as_u64().to_be_bytes()[*offset..*offset + copied],
                    );
                    *offset += copied;
                }
                Ok(copied + records.read_current(&mut output[copied..])?)
            }
            _ => Err(BackupError::Finished),
        }
    }

    fn finish_call<T>(&mut self, result: &BackupResult<T>) {
        if let Some(records) = &self.records {
            self.positions = records.visited_positions();
            self.copied_bytes = records.copied_bytes();
        }
        if result.is_err() && !matches!(self.stage, Stage::Complete | Stage::Failed) {
            self.release(Stage::Failed);
        }
    }

    fn release(&mut self, stage: Stage) {
        if let Some(records) = &self.records {
            self.positions = records.visited_positions();
            self.copied_bytes = records.copied_bytes();
        }
        self.stage = stage;
        self.records = None;
        self.source = None;
        self.notification_key = BackupCursorKey::new(self.control.memory());
    }
}
