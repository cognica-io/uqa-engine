//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use crate::{
    backup::{
        BackupError, BackupImageSource, BackupLimits, BackupResult, BackupSourceStatistics,
        ImageDigest, InventoryEntry, InventorySummary, NamespaceId, ObjectSpan, UnitAddress,
        UnitKind,
    },
    read_control::StorageReadControl,
    temporary_file::{FileBudget, FileLimit},
};

use super::{BackupOperation, PackObject, PackRange, State};

mod job;
mod parent;
pub use job::{BackupCapturePhase, BackupCaptureProgress, BackupCaptureStep, BackupImageCapture};

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
    fn address(&self) -> UnitAddress<'_> {
        UnitAddress {
            namespace: self.namespace,
            generation: self.generation,
            family: self.family,
            key: &self.key,
        }
    }

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
            self.address(),
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
        let result = self.capture_image(
            source,
            &FileBudget::new(maximum_workspace_bytes),
            None,
            control,
        );
        if result.is_err() {
            self.state = State::Failed;
        }
        result.map_err(workspace_error)
    }

    pub(super) fn capture_image(
        &mut self,
        source: impl BackupImageSource,
        files: &FileBudget,
        expected: Option<InventorySummary>,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCaptureReport> {
        job::run(self, source, files, expected, control)
    }
}

pub(super) fn workspace_error(error: BackupError) -> BackupError {
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
