//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared ordered image streaming; provider mappings own wire headers and membership.

use std::sync::Arc;

use crate::{
    backup::{
        BackupCut, BackupError, BackupImageSource, BackupResult, BackupSourceStep,
        BackupSourceUnit, ImageFormat, NamespaceId, RecordImageCursor, RecordImageFamily,
        RecordImageStep, SnapshotId, UnitAddress, UnitKind,
    },
    mvcc::{CommittedBackupSnapshot, CommittedRecordSnapshot},
    read_control::StorageReadControl,
};

/// Installed mapping policy for one complete record image. Header and namespace are immutable for this stream. Membership must read only the supplied committed cut, perform bounded metadata work and never fetch an excluded unit's payload. Returned metadata bytes describe actual reads, independently of payload streaming.
pub trait BackupRecordMapping {
    fn format(&self) -> ImageFormat;
    fn namespace(&self) -> NamespaceId;
    fn header(&self) -> &[u8];
    fn include(
        &self,
        records: &dyn CommittedRecordSnapshot,
        key: &[u8],
        control: &StorageReadControl,
    ) -> BackupResult<(bool, usize)>;
}

enum Stage {
    Ready,
    Header(usize),
    Scan,
    Unit(usize),
    Complete,
    Failed,
}

/// Ordered record-image stream over one joint cut, with membership and wire identity supplied by its owning mapping.
pub struct MappedRecordBackupImage<M: BackupRecordMapping> {
    cut: BackupCut,
    source: Option<Arc<dyn CommittedBackupSnapshot>>,
    records: Option<RecordImageCursor>,
    mapping: Option<M>,
    control: StorageReadControl,
    stage: Stage,
    positions: u64,
    copied_bytes: u64,
    metadata_read_bytes: u64,
    excluded_units: u64,
}

impl<M: BackupRecordMapping> MappedRecordBackupImage<M> {
    /// The mapping owner supplies the exact provider format, header and retained-cut membership. This shared stream owns cursor consumption and terminal resource retirement.
    pub fn from_mapping(
        source: Arc<dyn CommittedBackupSnapshot>,
        mapping: M,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if mapping.header().is_empty() {
            return Err(BackupError::Invalid("missing record image header"));
        }
        let cut = BackupCut {
            source: source.database_id(),
            snapshot: SnapshotId::new()?,
            format: mapping.format(),
            sequence: Some(source.records().sequence()),
        };
        let records = RecordImageCursor::new(source.clone(), control)?;
        Ok(Self {
            cut,
            source: Some(source),
            records: Some(records),
            mapping: Some(mapping),
            control: control.clone(),
            stage: Stage::Ready,
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

    pub fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        let mapping = self.mapping.as_ref()?;
        let (family, key, kind, decoded_bytes) = match self.stage {
            Stage::Header(_) => (
                1,
                &[][..],
                UnitKind::Metadata,
                mapping.header().len() as u64,
            ),
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
        Some(BackupSourceUnit {
            address: UnitAddress {
                namespace: mapping.namespace(),
                generation: 0,
                family,
                key,
            },
            kind,
            decoded_bytes,
        })
    }

    /// Inspect at most `positions` raw positions and at most one generation-state record. An excluded unit returns Pending without reading its payload. Every physical read closes before return, including when a selected unit still has unread bytes.
    pub fn advance(&mut self, positions: usize) -> BackupResult<BackupSourceStep> {
        let result = self.advance_inner(positions);
        self.finish_call(&result);
        result
    }

    fn advance_inner(&mut self, positions: usize) -> BackupResult<BackupSourceStep> {
        match self.stage {
            Stage::Complete => return Ok(BackupSourceStep::Complete),
            Stage::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        self.control.cancellation().check()?;
        if positions == 0 {
            return Err(BackupError::Invalid("zero record image scan window"));
        }
        match self.stage {
            Stage::Ready => {
                self.stage = Stage::Header(0);
                return Ok(BackupSourceStep::Unit);
            }
            Stage::Header(offset) if offset != self.mapping.as_ref().unwrap().header().len() => {
                return Err(BackupError::Invalid("record image header is not consumed"));
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
                    "record image revision is not consumed",
                ));
            }
            _ => (),
        }
        let records = self.records.as_mut().expect("active image owns cursor");
        match records.advance(positions)? {
            RecordImageStep::Complete => {
                self.release(Stage::Complete);
                Ok(BackupSourceStep::Complete)
            }
            RecordImageStep::Pending => {
                self.stage = Stage::Scan;
                Ok(BackupSourceStep::Pending)
            }
            RecordImageStep::Unit => {
                let unit = records.current_unit().expect("selected raw unit");
                let (include, bytes) = match unit.family {
                    RecordImageFamily::IdentifierWatermark => (true, 0),
                    RecordImageFamily::Record => self.mapping.as_ref().unwrap().include(
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
                    return Ok(BackupSourceStep::Pending);
                }
                if unit.family == RecordImageFamily::Record {
                    (unit.decoded_bytes as u64)
                        .checked_add(8)
                        .ok_or(BackupError::Invalid("record image record length overflow"))?;
                }
                self.stage = Stage::Unit(0);
                Ok(BackupSourceStep::Unit)
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
                let header = self.mapping.as_ref().unwrap().header();
                let size = output.len().min(header.len() - *offset);
                output[..size].copy_from_slice(&header[*offset..*offset + size]);
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
        self.mapping = None;
    }
}

impl<M: BackupRecordMapping> BackupImageSource for MappedRecordBackupImage<M> {
    fn cut(&self) -> BackupCut {
        self.cut()
    }

    fn advance(
        &mut self,
        maximum_positions: usize,
    ) -> BackupResult<crate::backup::BackupSourceStep> {
        self.advance(maximum_positions)
    }

    fn current_unit(&self) -> Option<crate::backup::BackupSourceUnit<'_>> {
        self.current_unit()
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.read_current(output)
    }

    fn statistics(&self) -> crate::backup::BackupSourceStatistics {
        crate::backup::BackupSourceStatistics {
            visited_positions: self.visited_positions(),
            copied_payload_bytes: self.copied_source_bytes(),
            metadata_read_bytes: self.metadata_read_bytes(),
            excluded_units: self.excluded_units(),
        }
    }
}
