//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Read;

use sha2::{Digest, Sha256};

use crate::read_control::StorageReadControl;
use crate::spill_map::{Map, Record};
use crate::temporary_file::FileBudget;
use crate::{StorageBackendError, StorageBackendResult};

use super::super::{
    BackupError, BackupResult, BackupRoot, FrameKind, ImageDigest, InventoryReader, ObjectSpan,
};
use super::objects::SpanReader;
use super::LocalRepository;

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
    fn new(maximum_workspace_bytes: u64, control: &StorageReadControl) -> Self {
        let resident = (control.memory().available() / 8).min(32 * 1024);
        let files = FileBudget::new(maximum_workspace_bytes);
        Self {
            objects: Map::with_file_budget(control.memory(), resident, &files),
            ranges: Map::with_file_budget(control.memory(), resident, &files),
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
        let reader = repository.span_reader(span, kind, first, control)?;
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

    fn finish(self, control: &StorageReadControl) -> BackupResult<()> {
        let mut previous = None;
        for entry in self.ranges.iter() {
            control.cancellation().check()?;
            let (key, end) = entry?;
            let ordinal = (key >> 64) as u64;
            let offset = key as u64;
            if previous.is_some_and(|(object, end)| object == ordinal && offset < end) {
                return Err(BackupError::Invalid("overlapping object frame ranges"));
            }
            previous = Some((ordinal, *end));
        }
        Ok(())
    }
}

impl LocalRepository {
    pub(super) fn verify_root(
        &self,
        root: BackupRoot,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        control.cancellation().check()?;
        if root.repository() != self.id {
            return Err(BackupError::Invalid("foreign repository root"));
        }
        let mut ledger = Ledger::new(maximum_workspace_bytes, control);
        let input = ledger.open(self, root.inventory(), FrameKind::Inventory, control)?;
        let mut inventory = InventoryReader::new(input, self.limits, control)?;
        if inventory.format() != root.cut().format {
            return Err(BackupError::Invalid(
                "inventory image format differs from root",
            ));
        }
        let mut buffer = [0; 16 * 1024];
        while let Some(entry) = inventory.read_next()? {
            let mut payload = ledger.open(self, entry.object(), FrameKind::Unit, control)?;
            let mut hash = Sha256::new();
            let mut length = 0_u64;
            loop {
                control.cancellation().check()?;
                let read = payload.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                length = length
                    .checked_add(read as u64)
                    .ok_or(BackupError::Invalid("unit size overflow"))?;
                if length > entry.decoded_bytes() {
                    return Err(BackupError::Integrity);
                }
                hash.update(&buffer[..read]);
            }
            if length != entry.decoded_bytes()
                || ImageDigest::from_bytes(hash.finalize().into()) != entry.digest()
            {
                return Err(BackupError::Integrity);
            }
        }
        if inventory.summary() != Some(root.summary()) {
            return Err(BackupError::Integrity);
        }
        drop(inventory);
        ledger.finish(control)
    }
}
