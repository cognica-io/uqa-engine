//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Seek, SeekFrom};

use uqa_core::memory::BudgetedVec;

use crate::{
    backup::{
        BackupError, BackupImageSource, BackupLimits, BackupResult, BackupSourceStatistics,
        BackupSourceStep, FrameKind, ImageDigest, InventoryEntry, InventorySummary,
        InventoryWriter, NamespaceId, ObjectSpan, UnitAddress, UnitKind,
    },
    read_control::StorageReadControl,
    temporary_file::{FileBudget, FileLimit, TemporaryFile},
};

use super::{BackupOperation, PackObject, PackRange, State};
use parent::ParentInventory;
use selected::{SelectedImage, SelectedUnit};

mod parent;
mod selected;

/// Logical capture work. Payload counters exclude repository framing and parent-inventory verification; written object bytes include new packs and the complete child inventory, not the publication descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupImageStatistics {
    pub parent_units: u64,
    pub hashed_payload_bytes: u64,
    pub reused_units: u64,
    pub reused_payload_bytes: u64,
    pub written_units: u64,
    pub written_payload_bytes: u64,
    pub written_object_bytes: u64,
}

impl BackupImageStatistics {
    fn payload(&mut self, bytes: u64, reused: bool) -> BackupResult<()> {
        add(&mut self.hashed_payload_bytes, bytes)?;
        if reused {
            add(&mut self.reused_units, 1)?;
            add(&mut self.reused_payload_bytes, bytes)
        } else {
            add(&mut self.written_units, 1)?;
            add(&mut self.written_payload_bytes, bytes)
        }
    }
}

fn add(value: &mut u64, increment: u64) -> BackupResult<()> {
    *value = value
        .checked_add(increment)
        .ok_or(BackupError::Invalid("image statistics overflow"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCaptureReport {
    pub inventory: InventorySummary,
    pub source: BackupSourceStatistics,
    pub image: BackupImageStatistics,
}

struct UnitMetadata {
    namespace: NamespaceId,
    generation: u64,
    family: u16,
    key: BudgetedVec<u8>,
    kind: UnitKind,
    bytes: u64,
}

impl UnitMetadata {
    fn charge(&self) -> BackupResult<usize> {
        self.key
            .len()
            .checked_add(size_of::<Pending>())
            .ok_or(BackupError::Invalid("image metadata size overflow"))
    }

    fn entry(
        &self,
        digest: ImageDigest,
        span: ObjectSpan,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<InventoryEntry> {
        InventoryEntry::new(
            UnitAddress {
                namespace: self.namespace,
                generation: self.generation,
                family: self.family,
                key: &self.key,
            },
            self.kind,
            self.bytes,
            digest,
            span,
            limits,
            control,
        )
    }
}

enum Reference {
    Written(PackRange),
    Reused(ObjectSpan),
}

struct Pending {
    metadata: UnitMetadata,
    digest: ImageDigest,
    reference: Reference,
}

impl Pending {
    fn entry(
        &self,
        object: PackObject,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<InventoryEntry> {
        let span = match self.reference {
            Reference::Written(range) => object.span(range)?,
            Reference::Reused(span) => span,
        };
        self.metadata.entry(self.digest, span, limits, control)
    }
}

impl BackupOperation<'_> {
    /// Consume a complete image with bounded source windows and encrypted workspace. A retained parent is merge-scanned by exact address, kind, length and SHA-256 image digest; equal units inherit immutable object ranges. Without a source fingerprint, comparison reads and hashes the current payload and temporarily spools possible matches. Publication remains explicit and owns its original operation identity.
    pub fn prepare_image(
        &mut self,
        source: impl BackupImageSource,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCaptureReport> {
        if !matches!(self.state, State::Capturing) {
            return Err(BackupError::Finished);
        }
        let result = self.capture_image(source, maximum_workspace_bytes, control);
        if result.is_err() {
            self.state = State::Failed;
        }
        result.map_err(workspace_error)
    }

    fn capture_image(
        &mut self,
        mut source: impl BackupImageSource,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCaptureReport> {
        control.cancellation().check()?;
        if source.cut() != self.cut() {
            return Err(BackupError::Invalid(
                "image source differs from capture cut",
            ));
        }
        let repository = self.repository;
        let limits = repository.limits;
        let mut parent = self
            .parent_lease
            .as_ref()
            .map(|lease| ParentInventory::new(repository, lease.root(), control))
            .transpose()?;
        let files = FileBudget::new(maximum_workspace_bytes);
        let spool = TemporaryFile::with_file_budget(&files)?;
        let mut inventory = InventoryWriter::new(spool, self.cut().format, limits, control)?;
        let mut statistics = BackupImageStatistics::default();
        let mut selected = select(&mut source, control)?;
        while selected {
            let mut unit =
                SelectedUnit::new(&mut source, parent.as_mut(), &files, limits, control)?;
            if let SelectedImage::Reused { digest, span } = &unit.image {
                inventory.append(&unit.metadata.entry(*digest, *span, limits, control)?)?;
                statistics.payload(unit.metadata.bytes, true)?;
                selected = select(&mut source, control)?;
                continue;
            }
            let mut pack = self.begin_pack(control)?;
            let mut pending = BudgetedVec::new(control.memory());
            let retention_limit = (control.memory().available() / 4).min(64 * 1024);
            let mut retained = 0_usize;
            let mut decoded = 0_u64;
            loop {
                pending.reserve(1)?;
                retained = retained
                    .checked_add(unit.metadata.charge()?)
                    .ok_or(BackupError::Invalid("image metadata size overflow"))?;
                decoded = decoded
                    .checked_add(unit.metadata.bytes)
                    .ok_or(BackupError::Invalid("pack size overflow"))?;
                pending.push(unit.append(&mut pack, &mut source, &mut statistics, control)?)?;
                selected = select(&mut source, control)?;
                if !selected || pending.len() >= 32 || decoded >= 64 * 1024 {
                    break;
                }
                let charge = source
                    .current_unit()
                    .ok_or(BackupError::Invalid("source has no selected image unit"))?
                    .address
                    .key
                    .len()
                    .checked_add(size_of::<Pending>())
                    .ok_or(BackupError::Invalid("image metadata size overflow"))?;
                if charge > retention_limit.saturating_sub(retained) {
                    break;
                }
                unit = SelectedUnit::new(&mut source, parent.as_mut(), &files, limits, control)?;
            }
            let object = pack.finish(control)?;
            add(&mut statistics.written_object_bytes, object.encoded_bytes())?;
            for unit in pending.iter() {
                inventory.append(&unit.entry(object, limits, control)?)?;
            }
        }
        let source_statistics = source.statistics();
        drop(source);
        statistics.parent_units = parent.map_or(Ok(0), ParentInventory::finish)?;
        let (mut spool, summary) = inventory.finish()?;
        spool.seek(SeekFrom::Start(0))?;
        let span = self.write_object(FrameKind::Inventory, &mut spool, control)?;
        add(&mut statistics.written_object_bytes, span.object_bytes())?;
        drop(spool);
        self.prepare(span, summary, control)?;
        Ok(BackupCaptureReport {
            inventory: summary,
            source: source_statistics,
            image: statistics,
        })
    }
}

fn select(source: &mut impl BackupImageSource, control: &StorageReadControl) -> BackupResult<bool> {
    loop {
        control.cancellation().check()?;
        match source.advance(128)? {
            BackupSourceStep::Unit => return Ok(true),
            BackupSourceStep::Pending => (),
            BackupSourceStep::Complete => return Ok(false),
        }
    }
}

fn workspace_error(error: BackupError) -> BackupError {
    if let BackupError::Io(io) = &error {
        if let Some(limit) = io
            .get_ref()
            .and_then(|error| error.downcast_ref::<FileLimit>())
        {
            return BackupError::Limit {
                resource: "capture workspace",
                required: limit.required,
                limit: limit.limit,
            };
        }
    }
    error
}
