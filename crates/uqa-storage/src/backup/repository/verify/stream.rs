//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental verification shares the repository's complete object/range ledger.

use super::{
    super::{directory, objects::SpanReader, LocalRepository},
    Ledger,
};
use crate::{
    backup::{
        BackupError, BackupResult, BackupRoot, FrameKind, ImageDigest, InventoryEntry,
        InventoryReader,
    },
    read_control::StorageReadControl,
    temporary_file::FileBudget,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupReadStep {
    /// Read this unit through its verified end before advancing.
    Unit,
    /// One bounded object or overlap-verification step completed.
    Pending,
    Complete,
}

/// Complete frame, unit, inventory, object and range verification. This is not a provider image-validation or target-activation certificate.
#[derive(Clone, Copy)]
pub struct VerifiedBackupImage(BackupRoot);

impl VerifiedBackupImage {
    pub const fn root(self) -> BackupRoot {
        self.0
    }
}

struct Unit<'a> {
    entry: InventoryEntry,
    input: SpanReader<'a>,
    hash: Sha256,
    bytes: u64,
    complete: bool,
}

struct Object {
    file: File,
    expected: ImageDigest,
    remaining: u64,
    hash: Sha256,
}

enum Stage {
    Inventory,
    Objects {
        after: Option<u128>,
    },
    Ranges {
        after: Option<u128>,
        previous: Option<(u64, u64)>,
    },
    Complete,
    Failed,
}

/// Stream authenticated units, then verify all whole objects and non-overlapping ranges. Payload bytes remain provisional until `verified` is available; importing owners must keep their target inaccessible until their own validation and activation also complete. Errors immediately release files, keys and encrypted verification workspace.
pub struct BackupImageReader<'a> {
    repository: &'a LocalRepository,
    root: BackupRoot,
    control: StorageReadControl,
    ledger: Option<Ledger>,
    inventory: Option<InventoryReader<SpanReader<'a>>>,
    unit: Option<Unit<'a>>,
    object: Option<Object>,
    stage: Stage,
    object_bytes: u64,
}

impl<'a> BackupImageReader<'a> {
    pub(super) fn new(
        repository: &'a LocalRepository,
        root: BackupRoot,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        Self::with_file_budget(
            repository,
            root,
            &FileBudget::new(maximum_workspace_bytes),
            control,
        )
    }

    pub(in crate::backup::repository) fn with_file_budget(
        repository: &'a LocalRepository,
        root: BackupRoot,
        files: &FileBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if root.repository() != repository.id {
            return Err(BackupError::Invalid("foreign repository root"));
        }
        let mut ledger = Ledger::new(files, control);
        let input = ledger.open(repository, root.inventory(), FrameKind::Inventory, control)?;
        let inventory = InventoryReader::new(input, repository.limits, control)?;
        if inventory.format() != root.cut().format {
            return Err(BackupError::Invalid(
                "inventory image format differs from root",
            ));
        }
        Ok(Self {
            repository,
            root,
            control: control.clone(),
            ledger: Some(ledger),
            inventory: Some(inventory),
            unit: None,
            object: None,
            stage: Stage::Inventory,
            object_bytes: 0,
        })
    }

    pub fn current_unit(&self) -> Option<&InventoryEntry> {
        self.unit.as_ref().map(|unit| &unit.entry)
    }

    pub fn verified(&self) -> Option<VerifiedBackupImage> {
        matches!(self.stage, Stage::Complete).then_some(VerifiedBackupImage(self.root))
    }

    /// Bytes hashed in the whole-object pass; frame/payload reads and OS page activity are separate.
    pub const fn verified_object_bytes(&self) -> u64 {
        self.object_bytes
    }

    /// Decode one bounded inventory entry, hash at most `maximum_object_bytes` (capped at 16 KiB), or check one ordered range. Inventory and frame size limits apply independently. No unit may be skipped.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupReadStep> {
        let result = self.advance_inner(maximum_object_bytes);
        self.failed(&result);
        result
    }

    fn advance_inner(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupReadStep> {
        match self.stage {
            Stage::Complete => return Ok(BackupReadStep::Complete),
            Stage::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        self.control.cancellation().check()?;
        if maximum_object_bytes == 0 {
            return Err(BackupError::Invalid("zero object verification window"));
        }
        if self.unit.as_ref().is_some_and(|unit| !unit.complete) {
            return Err(BackupError::Invalid("backup image unit is not consumed"));
        }
        self.unit = None;
        match self.stage {
            Stage::Inventory => self.next_unit(),
            Stage::Objects { after } => self.next_object(after, maximum_object_bytes),
            Stage::Ranges { after, previous } => {
                let ledger = self.ledger.as_ref().expect("active reader owns ledger");
                if let Some((key, end)) = ledger.ranges.next(after)? {
                    let ordinal = (key >> 64) as u64;
                    let offset = key as u64;
                    if previous.is_some_and(|(object, end)| object == ordinal && offset < end) {
                        return Err(BackupError::Invalid("overlapping object frame ranges"));
                    }
                    self.stage = Stage::Ranges {
                        after: Some(key),
                        previous: Some((ordinal, *end)),
                    };
                    Ok(BackupReadStep::Pending)
                } else {
                    self.release(Stage::Complete);
                    Ok(BackupReadStep::Complete)
                }
            }
            Stage::Complete | Stage::Failed => unreachable!("terminal states returned above"),
        }
    }

    fn next_unit(&mut self) -> BackupResult<BackupReadStep> {
        let inventory = self.inventory.as_mut().expect("inventory stage owns input");
        if let Some(entry) = inventory.read_next()? {
            let input = self
                .ledger
                .as_mut()
                .expect("active reader owns ledger")
                .open(
                    self.repository,
                    entry.object(),
                    FrameKind::Unit,
                    &self.control,
                )?;
            self.unit = Some(Unit {
                entry,
                input,
                hash: Sha256::new(),
                bytes: 0,
                complete: false,
            });
            return Ok(BackupReadStep::Unit);
        }
        if inventory.summary() != Some(self.root.summary()) {
            return Err(BackupError::Integrity);
        }
        self.inventory = None;
        self.stage = Stage::Objects { after: None };
        Ok(BackupReadStep::Pending)
    }

    fn next_object(&mut self, after: Option<u128>, maximum: usize) -> BackupResult<BackupReadStep> {
        if self.object.is_none() {
            let ledger = self.ledger.as_ref().expect("active reader owns ledger");
            let Some((id, object)) = ledger.objects.next(after)? else {
                self.stage = Stage::Ranges {
                    after: None,
                    previous: None,
                };
                return Ok(BackupReadStep::Pending);
            };
            let name = directory::name(id.to_be_bytes());
            let file =
                directory::open_file(&self.repository.objects, directory::text(&name), false)?;
            if file.metadata()?.len() != object.bytes {
                return Err(BackupError::Integrity);
            }
            self.object = Some(Object {
                file,
                expected: object.digest,
                remaining: object.bytes,
                hash: Sha256::new(),
            });
            self.stage = Stage::Objects { after: Some(id) };
        }
        let object = self.object.as_mut().expect("selected object");
        let mut buffer = [0; 16 * 1024];
        let size = object.remaining.min(buffer.len().min(maximum) as u64) as usize;
        let read = match object.file.read(&mut buffer[..size]) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Ok(BackupReadStep::Pending)
            }
            Err(error) => return Err(error.into()),
        };
        if read == 0 && object.remaining != 0 {
            return Err(BackupError::Integrity);
        }
        object.hash.update(&buffer[..read]);
        object.remaining -= read as u64;
        self.object_bytes = self
            .object_bytes
            .checked_add(read as u64)
            .ok_or(BackupError::Invalid("object byte count overflow"))?;
        if object.remaining == 0 {
            super::super::super::frame::require_end(&mut object.file, &self.control)?;
            if ImageDigest::from_bytes(object.hash.clone().finalize().into()) != object.expected {
                return Err(BackupError::Integrity);
            }
            self.object = None;
        }
        Ok(BackupReadStep::Pending)
    }

    /// Copy at most one decoded frame range into a nonempty caller buffer. Zero means the unit's exact length, digest and frame end have been verified. Returned bytes must not activate a target before complete image and provider validation.
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = self.read_inner(output);
        self.failed(&result);
        result
    }

    fn read_inner(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let unit = self.unit.as_mut().ok_or(BackupError::Finished)?;
        if unit.complete {
            return Ok(0);
        }
        self.control.cancellation().check()?;
        if output.is_empty() {
            return Err(BackupError::Invalid("empty image read buffer"));
        }
        let read = unit.input.read(output)?;
        unit.bytes = unit
            .bytes
            .checked_add(read as u64)
            .ok_or(BackupError::Invalid("unit size overflow"))?;
        if unit.bytes > unit.entry.decoded_bytes() {
            return Err(BackupError::Integrity);
        }
        unit.hash.update(&output[..read]);
        if read == 0 {
            if unit.bytes != unit.entry.decoded_bytes()
                || ImageDigest::from_bytes(unit.hash.clone().finalize().into())
                    != unit.entry.digest()
            {
                return Err(BackupError::Integrity);
            }
            unit.complete = true;
        }
        Ok(read)
    }

    fn failed<T>(&mut self, result: &BackupResult<T>) {
        if result.is_err() && !matches!(self.stage, Stage::Complete | Stage::Failed) {
            self.release(Stage::Failed);
        }
    }

    fn release(&mut self, stage: Stage) {
        self.stage = stage;
        self.unit = None;
        self.object = None;
        self.inventory = None;
        self.ledger = None;
    }
}
