//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{self, Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use crate::{
    backup::{
        BackupError, BackupImageSource, BackupLimits, BackupResult, BackupSourceStatistics,
        BackupSourceStep, BackupSourceUnit, FrameKind, ImageDigest, InventoryEntry,
        InventorySummary, InventoryWriter, NamespaceId, UnitAddress, UnitKind,
    },
    read_control::StorageReadControl,
    temporary_file::{FileBudget, FileLimit, TemporaryFile},
};

use super::{BackupOperation, PackObject, PackRange, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCaptureReport {
    pub inventory: InventorySummary,
    pub source: BackupSourceStatistics,
}

struct Pending {
    namespace: NamespaceId,
    generation: u64,
    family: u16,
    key: BudgetedVec<u8>,
    kind: UnitKind,
    bytes: u64,
    digest: ImageDigest,
    range: PackRange,
}

impl Pending {
    fn entry(
        &self,
        object: PackObject,
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
            self.digest,
            object.span(self.range)?,
            limits,
            control,
        )
    }
}

struct Payload<'a, S> {
    source: &'a mut S,
    hash: Sha256,
    bytes: u64,
    expected: u64,
}

impl<S: BackupImageSource> Read for Payload<'_, S> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let result = (|| {
            let read = self.source.read_current(output)?;
            if read > output.len() {
                return Err(BackupError::Invalid("source read exceeds output buffer"));
            }
            self.bytes = self
                .bytes
                .checked_add(read as u64)
                .ok_or(BackupError::Integrity)?;
            if self.bytes > self.expected || (read == 0 && self.bytes != self.expected) {
                return Err(BackupError::Integrity);
            }
            self.hash.update(&output[..read]);
            Ok(read)
        })();
        result.map_err(io::Error::other)
    }
}

impl BackupOperation<'_> {
    /// Consume and prepare a complete image with bounded source windows, shared packs and an encrypted inventory spool. This synchronous operation owns the source until success or failure. Publication remains explicit and can be resolved through this operation's existing identity. `maximum_workspace_bytes` limits the spool's physical ciphertext; the operation's encoded staging limit separately bounds repository objects and verification.
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
        let limits = self.repository.limits;
        let files = FileBudget::new(maximum_workspace_bytes);
        let spool = TemporaryFile::with_file_budget(&files)?;
        let mut inventory = InventoryWriter::new(spool, self.cut().format, limits, control)?;
        let mut selected = select(&mut source, control)?;
        while selected {
            let mut pack = self.begin_pack(control)?;
            let mut pending = BudgetedVec::new(control.memory());
            let retention_limit = (control.memory().available() / 4).min(64 * 1024);
            let mut retained = 0_usize;
            let mut decoded = 0_u64;
            loop {
                let unit = source
                    .current_unit()
                    .ok_or(BackupError::Invalid("source has no selected image unit"))?;
                let charge = unit
                    .address
                    .key
                    .len()
                    .checked_add(size_of::<Pending>())
                    .ok_or(BackupError::Invalid("image metadata size overflow"))?;
                if !pending.is_empty() && charge > retention_limit.saturating_sub(retained) {
                    break;
                }
                limits.check_key(unit.address.key.len())?;
                let mut key = BudgetedVec::new(control.memory());
                key.extend_from_slice(unit.address.key)?;
                let BackupSourceUnit {
                    address,
                    kind,
                    decoded_bytes,
                } = unit;
                let (namespace, generation, family) =
                    (address.namespace, address.generation, address.family);
                pending.reserve(1)?;
                let mut input = Payload {
                    source: &mut source,
                    hash: Sha256::new(),
                    bytes: 0,
                    expected: decoded_bytes,
                };
                let range = pack.append(FrameKind::Unit, &mut input, control)?;
                if input.bytes != decoded_bytes {
                    return Err(BackupError::Integrity);
                }
                let digest = ImageDigest::from_bytes(input.hash.finalize().into());
                pending.push(Pending {
                    namespace,
                    generation,
                    family,
                    key,
                    kind,
                    bytes: decoded_bytes,
                    digest,
                    range,
                })?;
                retained = retained
                    .checked_add(charge)
                    .ok_or(BackupError::Invalid("image metadata size overflow"))?;
                decoded = decoded
                    .checked_add(decoded_bytes)
                    .ok_or(BackupError::Invalid("pack size overflow"))?;
                selected = select(&mut source, control)?;
                if !selected || pending.len() >= 32 || decoded >= 64 * 1024 {
                    break;
                }
            }
            let object = pack.finish(control)?;
            for unit in pending.iter() {
                let entry = unit.entry(object, limits, control)?;
                inventory.append(&entry)?;
            }
        }
        let statistics = source.statistics();
        drop(source);
        let (mut spool, summary) = inventory.finish()?;
        spool.seek(SeekFrom::Start(0))?;
        let span = self.write_object(FrameKind::Inventory, &mut spool, control)?;
        drop(spool);
        self.prepare(span, summary, control)?;
        Ok(BackupCaptureReport {
            inventory: summary,
            source: statistics,
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
