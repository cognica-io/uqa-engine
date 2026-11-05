//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    backup::{
        BackupError, BackupResult, BackupRoot, FrameKind, ImageDigest, InventoryReader, ObjectSpan,
    },
    read_control::StorageReadControl,
    spill_map::{Map, Record},
    temporary_file::FileBudget,
    StorageBackendResult,
};

use super::super::{directory, LocalRepository};

struct Object {
    ordinal: u64,
    bytes: u64,
    digest: ImageDigest,
    referenced: bool,
}

impl Record for Object {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(49)
    }
    fn encode(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.ordinal.to_le_bytes());
        output.extend_from_slice(&self.bytes.to_le_bytes());
        output.extend_from_slice(&self.digest.as_bytes());
        output.push(u8::from(self.referenced));
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 49 || bytes[48] > 1 {
            return Err(crate::spill_map::invalid("backup reachability object"));
        }
        Ok(Self {
            ordinal: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            bytes: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            digest: ImageDigest::from_bytes(bytes[16..48].try_into().unwrap()),
            referenced: bytes[48] != 0,
        })
    }
}

pub(super) struct Reachability {
    objects: Map<Object>,
    ranges: Map<u64>,
}

impl Reachability {
    pub(super) fn new(files: &FileBudget, control: &StorageReadControl) -> Self {
        let resident = (control.memory().available() / 8).min(32 * 1024);
        Self {
            objects: Map::with_file_budget(control.memory(), resident, files),
            ranges: Map::with_file_budget(control.memory(), resident, files),
        }
    }

    pub(super) fn contains(&self, id: [u8; 16]) -> BackupResult<bool> {
        Ok(self.objects.contains_key(u128::from_be_bytes(id))?)
    }

    /// An active capture may own both installed and unfinished private packs.
    /// Publication changes the repository generation before a new installation.
    pub(super) fn retain_pending(
        &mut self,
        id: [u8; 16],
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let key = u128::from_be_bytes(id);
        if !self.objects.contains_key(key)? {
            self.objects.insert(
                key,
                Object {
                    ordinal: self.objects.len() as u64,
                    bytes: 0,
                    digest: ImageDigest::from_bytes([0; 32]),
                    referenced: false,
                },
                Some(control),
            )?;
        }
        Ok(())
    }

    fn retain_span(&mut self, span: ObjectSpan, control: &StorageReadControl) -> BackupResult<()> {
        let key = u128::from_be_bytes(span.object().as_bytes());
        let previous = self.objects.get(key)?;
        let ordinal = previous
            .as_ref()
            .map_or(self.objects.len() as u64, |value| value.ordinal);
        let known = previous.as_ref().is_some_and(|value| value.referenced);
        if let Some(previous) = previous.filter(|value| value.referenced) {
            if previous.bytes != span.object_bytes() || previous.digest != span.digest() {
                return Err(BackupError::Integrity);
            }
        }
        if !known {
            self.objects.insert(
                key,
                Object {
                    ordinal,
                    bytes: span.object_bytes(),
                    digest: span.digest(),
                    referenced: true,
                },
                Some(control),
            )?;
        }
        let key = (u128::from(ordinal) << 64) | u128::from(span.offset());
        let end = span.offset() + span.encoded_bytes();
        if self.ranges.get(key)?.is_none_or(|previous| *previous < end) {
            self.ranges.insert(key, end, Some(control))?;
        }
        Ok(())
    }

    /// Root and complete inventory authentication establish the referenced set.
    /// Collection does not read unit payloads or certify their image integrity.
    pub(super) fn retain_root(
        &mut self,
        repository: &LocalRepository,
        root: BackupRoot,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.retain_span(root.inventory(), control)?;
        let input =
            repository.span_reader(root.inventory(), FrameKind::Inventory, true, control)?;
        let mut inventory = InventoryReader::new(input, repository.limits, control)?;
        if inventory.format() != root.cut().format {
            return Err(BackupError::Integrity);
        }
        while let Some(entry) = inventory.read_next()? {
            self.retain_span(entry.object(), control)?;
        }
        if inventory.summary() != Some(root.summary()) {
            return Err(BackupError::Integrity);
        }
        Ok(())
    }

    pub(super) fn validate_objects(
        &self,
        repository: &LocalRepository,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        for entry in self.objects.iter() {
            control.cancellation().check()?;
            let (id, object) = entry?;
            if object.referenced {
                let name = directory::name(id.to_be_bytes());
                let file =
                    directory::open_file(&repository.objects, directory::text(&name), false)?;
                if file.metadata()?.len() != object.bytes {
                    return Err(BackupError::Integrity);
                }
            }
        }
        Ok(())
    }

    /// Count the union of referenced encoded ranges, including inventories. A
    /// range shared by several roots contributes once; partial packs stay whole.
    pub(super) fn frame_bytes(&self, control: &StorageReadControl) -> BackupResult<u64> {
        let mut previous: Option<(u64, u64)> = None;
        let mut bytes = 0_u64;
        for entry in self.ranges.iter() {
            control.cancellation().check()?;
            let (key, end) = entry?;
            let object = (key >> 64) as u64;
            let start = key as u64;
            let start = previous
                .filter(|(id, _)| *id == object)
                .map_or(start, |(_, previous_end)| start.max(previous_end));
            bytes = bytes
                .checked_add(end.saturating_sub(start))
                .ok_or(BackupError::Invalid("retained byte count overflow"))?;
            previous = Some((object, start.max(*end)));
        }
        Ok(bytes)
    }
}
