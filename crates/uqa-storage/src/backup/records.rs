//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete live-record and allocator input streams over an already retained joint cut.

use super::{BackupError, BackupResult, UnitFingerprint};
use crate::{
    mvcc::{
        BackupCursorKey, CommitSequence, CommittedBackupSnapshot, DatabaseId, RecordMetadata,
        VersionResult,
    },
    read_control::StorageReadControl,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// SHA-256 over the record mapping's revision prefix and untagged value bytes.
/// The same accumulator is used for already-owned writes and streamed imports.
pub struct RecordImageHasher(Sha256);

impl RecordImageHasher {
    pub fn new(revision: CommitSequence) -> Self {
        let mut hash = Sha256::new();
        hash.update(revision.as_u64().to_be_bytes());
        Self(hash)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> super::ImageDigest {
        super::ImageDigest::from_bytes(self.0.finalize().into())
    }

    pub fn for_value(
        revision: CommitSequence,
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<super::ImageDigest> {
        let mut hash = Self::new(revision);
        for chunk in value.chunks(16 * 1024) {
            control.check()?;
            hash.update(chunk);
        }
        control.check()?;
        Ok(hash.finish())
    }
}

/// The provider image adapter assigns qualified namespaces and the final wire format to these raw input families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordImageFamily {
    IdentifierWatermark,
    Record,
}

/// One selected image input. Watermarks are eight big-endian bytes; records preserve their existing encoded bytes. This descriptor does not assert complete Engine state coverage.
#[derive(Clone, Copy)]
pub struct RecordImageUnit<'a> {
    pub family: RecordImageFamily,
    pub key: &'a [u8],
    pub decoded_bytes: usize,
    pub revision: Option<CommitSequence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordImageStep {
    /// Read this unit completely before advancing again; a zero-length unit is already consumed.
    Unit,
    /// A finite scan window ended without a visible unit, or the next family is ready.
    Pending,
    Complete,
}

#[derive(Clone, Copy)]
enum Plane {
    Identifiers,
    Records,
}

#[derive(Clone, Copy)]
enum Payload {
    Watermark(u64),
    Record(CommitSequence),
}

#[derive(Clone, Copy)]
struct Unit {
    plane: Plane,
    payload: Payload,
    length: usize,
    offset: usize,
    fingerprint: Option<UnitFingerprint>,
}

#[derive(Clone, Copy)]
enum Stage {
    Scan(Plane),
    ExactRecord,
    Unit(Unit),
    Complete,
    Failed,
}

#[derive(Clone, Copy)]
pub(super) enum RecordSelection<'a> {
    Descendants(&'a [u8]),
    Exact(&'a [u8]),
}

/// Streams only the joint cut's MVCC records and autonomous watermarks. Provider adapters remain responsible for complete image coverage, versioned encoding and import. The cursor owns the cut until completion or failure, and closes every source read before returning to its caller.
pub struct RecordImageCursor {
    source: Option<Arc<dyn CommittedBackupSnapshot>>,
    source_id: DatabaseId,
    sequence: CommitSequence,
    control: StorageReadControl,
    records_end: Option<BackupCursorKey>,
    identifiers_end: Option<BackupCursorKey>,
    records_end_exclusive: bool,
    maximum_key_bytes: usize,
    after: Option<BackupCursorKey>,
    stage: Stage,
    positions: u64,
    copied_bytes: u64,
    fingerprint_bytes: u64,
}

impl RecordImageCursor {
    pub fn new(
        source: Arc<dyn CommittedBackupSnapshot>,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        let records_end = source.record_key_ceiling(control)?;
        let identifiers_end = source.identifier_namespace_ceiling(control)?;
        Ok(Self {
            source_id: source.database_id(),
            sequence: source.records().sequence(),
            source: Some(source),
            control: control.clone(),
            records_end,
            identifiers_end,
            records_end_exclusive: false,
            maximum_key_bytes: usize::MAX,
            after: None,
            stage: Stage::Scan(Plane::Identifiers),
            positions: 0,
            copied_bytes: 0,
            fingerprint_bytes: 0,
        })
    }

    /// Select one physical key or the proper descendants of a prefix on this captured cut.
    pub(super) fn record_selection(
        &self,
        selection: RecordSelection<'_>,
        maximum_key_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let (prefix, stage, ceiling) = match selection {
            RecordSelection::Descendants(prefix) => (
                prefix,
                Stage::Scan(Plane::Records),
                self.records_end.as_deref(),
            ),
            RecordSelection::Exact(key) => (key, Stage::ExactRecord, None),
        };
        control.check_value_size(prefix.len(), maximum_key_bytes)?;
        let source = self.source.as_ref().ok_or(BackupError::Finished)?;
        let mut after = BackupCursorKey::new(control.memory());
        after.extend_from_slice(prefix)?;
        let mut exclusive = false;
        let mut end = None;
        if let Some(ceiling) = ceiling.filter(|end| *end > prefix) {
            let mut key = BackupCursorKey::new(control.memory());
            if ceiling.starts_with(prefix) {
                control.check_value_size(ceiling.len(), maximum_key_bytes)?;
                key.extend_from_slice(ceiling)?;
            } else {
                let last = prefix
                    .iter()
                    .rposition(|&byte| byte != u8::MAX)
                    .ok_or(BackupError::Invalid("invalid record prefix ceiling"))?;
                key.extend_from_slice(&prefix[..=last])?;
                key[last] += 1;
                exclusive = true;
            }
            end = Some(key);
        }
        Ok(Self {
            source: Some(source.clone()),
            source_id: self.source_id,
            sequence: self.sequence,
            control: control.clone(),
            records_end: end,
            identifiers_end: None,
            records_end_exclusive: exclusive,
            maximum_key_bytes,
            after: Some(after),
            stage,
            positions: 0,
            copied_bytes: 0,
            fingerprint_bytes: 0,
        })
    }

    pub(super) fn cancellation(&self) -> &uqa_core::CancellationToken {
        self.control.cancellation()
    }

    pub const fn source_id(&self) -> DatabaseId {
        self.source_id
    }
    pub const fn sequence(&self) -> CommitSequence {
        self.sequence
    }
    /// Positions delivered by the source's bounded visitors, including excluded keys. This is not page I/O.
    pub const fn visited_positions(&self) -> u64 {
        self.positions
    }
    /// Bytes successfully copied to the caller's output. This is not a physical disk-read counter.
    pub const fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    pub const fn fingerprint_bytes(&self) -> u64 {
        self.fingerprint_bytes
    }

    pub fn current_unit(&self) -> Option<RecordImageUnit<'_>> {
        let Stage::Unit(unit) = self.stage else {
            return None;
        };
        Some(RecordImageUnit {
            family: match unit.payload {
                Payload::Watermark(_) => RecordImageFamily::IdentifierWatermark,
                Payload::Record(_) => RecordImageFamily::Record,
            },
            key: self.after.as_deref().expect("a selected unit owns its key"),
            decoded_bytes: unit.length,
            revision: match unit.payload {
                Payload::Record(revision) => Some(revision),
                Payload::Watermark(_) => None,
            },
        })
    }

    pub(super) fn current_fingerprint(&self) -> Option<UnitFingerprint> {
        match self.stage {
            Stage::Unit(unit) => unit.fingerprint,
            _ => None,
        }
    }

    /// Omit the selected raw input without reading its remaining payload. Image owners use this only after classifying the unit at this same cut. Advancing still counts every inspected physical position; omitted bytes never enter the copied-byte count. Misuse or cancellation terminates the cursor and releases its cut.
    pub fn skip_current(&mut self) -> BackupResult<()> {
        let result = (|| {
            let Stage::Unit(unit) = self.stage else {
                return Err(BackupError::Finished);
            };
            self.control.cancellation().check()?;
            self.stage = Stage::Scan(unit.plane);
            Ok(())
        })();
        if result.is_err() && !matches!(self.stage, Stage::Complete | Stage::Failed) {
            self.release(Stage::Failed);
        }
        result
    }

    /// Inspect at most `positions` physical keys, closing each source read window before returning. The caller can write a completed chunk to its repository or yield between calls. Any error terminates this cursor and releases its cut and scan buffers.
    pub fn advance(&mut self, positions: usize) -> BackupResult<RecordImageStep> {
        let result = self.advance_inner(positions);
        if result.is_err() && !matches!(self.stage, Stage::Complete | Stage::Failed) {
            self.release(Stage::Failed);
        }
        result
    }

    fn advance_inner(&mut self, positions: usize) -> BackupResult<RecordImageStep> {
        match self.stage {
            Stage::Complete => return Ok(RecordImageStep::Complete),
            Stage::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        self.control.cancellation().check()?;
        if positions == 0 {
            return Err(BackupError::Invalid("zero record image scan window"));
        }
        let plane = match self.stage {
            Stage::Complete => return Ok(RecordImageStep::Complete),
            Stage::Failed => return Err(BackupError::Finished),
            Stage::Unit(unit) if unit.offset == unit.length => unit.plane,
            Stage::ExactRecord => return self.select_exact_record(),
            Stage::Unit(_) => {
                return Err(BackupError::Invalid("record image unit is not consumed"))
            }
            Stage::Scan(plane) => plane,
        };
        self.stage = Stage::Scan(plane);
        let end = match plane {
            Plane::Identifiers => self.identifiers_end.as_deref(),
            Plane::Records => self.records_end.as_deref(),
        };
        let Some(end) = end else {
            return Ok(self.end_plane(plane));
        };
        let source = self.source.as_ref().expect("active cursor owns its source");
        let mut next = None;
        let mut selected = None;
        let mut fingerprint = None;
        let mut fingerprint_bytes = 0_u64;
        let mut ended = false;
        let mut count = 0_u64;
        let mut visit = |key: &[u8], payload: Option<Payload>| -> VersionResult<bool> {
            count += 1;
            if key > end || (self.records_end_exclusive && key == end) {
                ended = true;
                return Ok(false);
            }
            if key.len() > self.maximum_key_bytes {
                self.control
                    .check_value_size(key.len(), self.maximum_key_bytes)?;
            }
            let mut retained = BackupCursorKey::new(self.control.memory());
            retained.extend_from_slice(key)?;
            next = Some(retained);
            selected = payload;
            Ok(selected.is_none())
        };
        let visited = match plane {
            Plane::Identifiers => source.visit_identifier_watermarks(
                self.after.as_deref(),
                positions,
                &self.control,
                &mut |key, value| visit(key, value.map(Payload::Watermark)),
            ),
            Plane::Records => source.visit_record_image_positions(
                self.after.as_deref(),
                positions,
                &self.control,
                &mut |key, image| {
                    fingerprint_bytes += u64::from(image.fingerprint_bytes);
                    fingerprint = image.fingerprint;
                    let value = image
                        .visible
                        .filter(|metadata| metadata.live)
                        .map(Self::record_payload)
                        .transpose()?;
                    visit(key, value)
                },
            ),
        };
        self.positions = self
            .positions
            .checked_add(count)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        self.fingerprint_bytes = self
            .fingerprint_bytes
            .checked_add(fingerprint_bytes)
            .ok_or(BackupError::Invalid(
                "fingerprint metadata counter overflow",
            ))?;
        visited?;
        if let Some(next) = next {
            self.after = Some(next);
        }
        if let Some(payload) = selected {
            self.select_unit(plane, payload, fingerprint)?;
            return Ok(RecordImageStep::Unit);
        }
        if ended || count < positions as u64 {
            return Ok(self.end_plane(plane));
        }
        Ok(RecordImageStep::Pending)
    }

    fn select_exact_record(&mut self) -> BackupResult<RecordImageStep> {
        let key = self.after.as_deref().expect("requested record key");
        let source = self.source.as_ref().ok_or(BackupError::Finished)?;
        let visible = source.records().metadata(key, &self.control)?;
        self.positions += 1;
        if let Some(metadata) = visible.filter(|metadata| metadata.live) {
            self.select_unit(Plane::Records, Self::record_payload(metadata)?, None)?;
            return Ok(RecordImageStep::Unit);
        }
        Ok(self.end_plane(Plane::Records))
    }

    fn record_payload(metadata: RecordMetadata) -> VersionResult<Payload> {
        metadata
            .revision
            .map(Payload::Record)
            .ok_or(crate::mvcc::VersionError::InvalidEncoding(
                "live image record has no revision",
            ))
    }

    fn select_unit(
        &mut self,
        plane: Plane,
        payload: Payload,
        fingerprint: Option<UnitFingerprint>,
    ) -> BackupResult<()> {
        let certified = fingerprint.and_then(|fp| {
            fp.decoded_bytes
                .checked_sub(8)
                .and_then(|n| usize::try_from(n).ok())
                .map(|length| (fp, length))
        });
        let (fingerprint, length) = match certified {
            Some((fingerprint, length)) => (Some(fingerprint), length),
            None => (None, self.selected_length(payload)?),
        };
        self.stage = Stage::Unit(Unit {
            plane,
            payload,
            length,
            offset: 0,
            fingerprint,
        });
        Ok(())
    }

    fn selected_length(&self, payload: Payload) -> BackupResult<usize> {
        Ok(match payload {
            Payload::Watermark(_) => 8,
            Payload::Record(revision) => {
                let chunk = self
                    .source
                    .as_ref()
                    .expect("active cursor owns its source")
                    .records()
                    .read_value_chunk(
                        self.after.as_deref().expect("selected key"),
                        0,
                        &mut [],
                        &self.control,
                    )?
                    .ok_or(BackupError::Invalid("selected image record is absent"))?;
                if chunk.revision() != revision || chunk.copied() != 0 {
                    return Err(BackupError::Invalid("selected image revision changed"));
                }
                chunk
                    .total_bytes()
                    .ok_or(BackupError::Invalid("selected image record is a tombstone"))?
            }
        })
    }

    /// Copy one range of the selected unit, closing the source read before return. The caller owns and accounts for `output`. On error its contents must be discarded, and this cursor is terminal.
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = self.read_inner(output);
        if result.is_err() && !matches!(self.stage, Stage::Complete | Stage::Failed) {
            self.release(Stage::Failed);
        }
        result
    }

    /// Copy an exact range from the selected record on its original revision. Field projections may skip unrelated bytes or revisit a prefix. Only bytes actually read contribute to the source counters.
    pub fn read_record_at(&mut self, offset: usize, output: &mut [u8]) -> BackupResult<()> {
        let Stage::Unit(mut unit) = self.stage else {
            if !matches!(self.stage, Stage::Complete | Stage::Failed) {
                self.release(Stage::Failed);
            }
            return Err(BackupError::Finished);
        };
        if !matches!(unit.plane, Plane::Records)
            || offset
                .checked_add(output.len())
                .is_none_or(|end| end > unit.length)
        {
            self.release(Stage::Failed);
            return Err(BackupError::Invalid("record projection exceeds its value"));
        }
        unit.offset = offset;
        self.stage = Stage::Unit(unit);
        self.read_current(output).map(|_| ())
    }

    fn read_inner(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let Stage::Unit(mut unit) = self.stage else {
            return Err(BackupError::Finished);
        };
        self.control.cancellation().check()?;
        let size = output.len().min(unit.length - unit.offset);
        match unit.payload {
            Payload::Watermark(value) => output[..size]
                .copy_from_slice(&value.to_be_bytes()[unit.offset..unit.offset + size]),
            Payload::Record(revision) => {
                let chunk = self
                    .source
                    .as_ref()
                    .expect("active cursor owns its source")
                    .records()
                    .read_value_chunk(
                        self.after.as_deref().expect("selected key"),
                        unit.offset,
                        &mut output[..size],
                        &self.control,
                    )?
                    .ok_or(BackupError::Invalid("image record disappeared"))?;
                if chunk.revision() != revision
                    || chunk.total_bytes() != Some(unit.length)
                    || chunk.copied() != size
                {
                    return Err(BackupError::Invalid("image record changed during export"));
                }
            }
        }
        self.control.cancellation().check()?;
        unit.offset += size;
        self.copied_bytes = self
            .copied_bytes
            .checked_add(size as u64)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        self.stage = Stage::Unit(unit);
        Ok(size)
    }

    fn end_plane(&mut self, plane: Plane) -> RecordImageStep {
        self.after = None;
        match plane {
            Plane::Identifiers => {
                self.identifiers_end = None;
                self.stage = Stage::Scan(Plane::Records);
                RecordImageStep::Pending
            }
            Plane::Records => {
                self.release(Stage::Complete);
                RecordImageStep::Complete
            }
        }
    }

    fn release(&mut self, stage: Stage) {
        self.stage = stage;
        self.source = None;
        self.records_end = None;
        self.identifiers_end = None;
        self.after = None;
    }
}
