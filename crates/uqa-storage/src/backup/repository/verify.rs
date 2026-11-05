//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::read_control::StorageReadControl;
use crate::spill_map::{Map, Record};
use crate::temporary_file::FileBudget;
use crate::{StorageBackendError, StorageBackendResult};

use super::super::{BackupError, BackupResult, BackupRoot, FrameKind, ImageDigest, ObjectSpan};
use super::objects::SpanReader;
use super::LocalRepository;

mod stream;
pub use stream::{BackupImageReader, BackupReadStep, VerifiedBackupImage};

/// Object identity is the complete 128-bit ID. Dense verification ordinals leave a full 64-bit offset in the independently spilled range key without truncating object identities.
struct VerifiedObject {
    ordinal: u64,
    bytes: u64,
    digest: ImageDigest,
}

impl Record for VerifiedObject {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(48)
    }
    fn encode(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.ordinal.to_le_bytes());
        output.extend_from_slice(&self.bytes.to_le_bytes());
        output.extend_from_slice(&self.digest.as_bytes());
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 48 {
            return Err(StorageBackendError::Other(
                "invalid backup verification record".into(),
            ));
        }
        Ok(Self {
            ordinal: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            bytes: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            digest: ImageDigest::from_bytes(bytes[16..].try_into().unwrap()),
        })
    }
}

struct Ledger {
    objects: Map<VerifiedObject>,
    ranges: Map<u64>,
}

impl Ledger {
    fn new(files: &FileBudget, control: &StorageReadControl) -> Self {
        let resident = (control.memory().available() / 8).min(32 * 1024);
        Self {
            objects: Map::with_file_budget(control.memory(), resident, files),
            ranges: Map::with_file_budget(control.memory(), resident, files),
        }
    }

    fn open<'a>(
        &mut self,
        repository: &'a LocalRepository,
        span: ObjectSpan,
        kind: FrameKind,
        control: &StorageReadControl,
    ) -> BackupResult<SpanReader<'a>> {
        let key = u128::from_be_bytes(span.object().as_bytes());
        let previous = self.objects.get(key)?;
        let (ordinal, first) = match previous {
            Some(previous) => {
                if previous.bytes != span.object_bytes() || previous.digest != span.digest() {
                    return Err(BackupError::Integrity);
                }
                (previous.ordinal, false)
            }
            None => (self.objects.len() as u64, true),
        };
        let range_key = (u128::from(ordinal) << 64) | u128::from(span.offset());
        if self.ranges.contains_key(range_key)? {
            return Err(BackupError::Invalid("duplicate object frame range"));
        }
        let reader = repository.span_reader(span, kind, false, control)?;
        if first {
            self.objects.insert(
                key,
                VerifiedObject {
                    ordinal,
                    bytes: span.object_bytes(),
                    digest: span.digest(),
                },
                Some(control),
            )?;
        }
        self.ranges.insert(
            range_key,
            span.offset() + span.encoded_bytes(),
            Some(control),
        )?;
        Ok(reader)
    }
}

pub(super) fn reader<'a>(
    repository: &'a LocalRepository,
    root: BackupRoot,
    maximum_workspace_bytes: u64,
    control: &StorageReadControl,
) -> BackupResult<BackupImageReader<'a>> {
    BackupImageReader::new(repository, root, maximum_workspace_bytes, control)
}

impl LocalRepository {
    pub(super) fn verify_root(
        &self,
        root: BackupRoot,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.verify_root_with_files(root, &FileBudget::new(maximum_workspace_bytes), control)
    }

    pub(super) fn verify_root_with_files(
        &self,
        root: BackupRoot,
        files: &FileBudget,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let mut input = BackupImageReader::with_file_budget(self, root, files, control)?;
        let mut buffer = [0; 16 * 1024];
        loop {
            match input.advance(buffer.len())? {
                BackupReadStep::Complete => return Ok(()),
                BackupReadStep::Pending => (),
                BackupReadStep::Unit => while input.read_current(&mut buffer)? != 0 {},
            }
        }
    }
}
