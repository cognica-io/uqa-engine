//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::read_control::StorageReadControl;

use super::super::frame::{read_exact, require_end, write_all};
use super::super::{BackupError, BackupResult, ObjectId};
use super::directory;
use super::LocalRepository;

impl LocalRepository {
    /// Called during exclusive non-destructive mutation. Collectors revalidate every publication, while preparation only needs to detect removal of verified references.
    pub(super) fn advance_generation(&self, control: &StorageReadControl) -> BackupResult<()> {
        self.write_generation(Some(self.removal_generation(control)?), control)
    }

    /// Change both identities before destructive mutation, or initialize an unpublished repository. Atomic replacement prevents mixed publication/removal epochs across processes.
    pub(super) fn advance_removal_generation(
        &self,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.write_generation(None, control)
    }

    fn write_generation(
        &self,
        removal: Option<[u8; 16]>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let generation = ObjectId::new()?.as_bytes();
        let mut name = [0; 44];
        name[..12].copy_from_slice(b".generation-");
        name[12..].copy_from_slice(&directory::name(generation));
        let name = std::str::from_utf8(&name).expect("fixed repository generation name");
        let mut pending = directory::create_file(&self.root, name)?;
        let mut bytes = [0; 40];
        bytes[..8].copy_from_slice(b"UQAGEN02");
        bytes[8..24].copy_from_slice(&generation);
        bytes[24..].copy_from_slice(&removal.unwrap_or(generation));
        // This bounded local coordination record contains no source data. Its atomic
        // replacement is not a replay-protection claim against repository rewriting.
        write_all(&mut pending, &bytes, control)?;
        pending.sync_all()?;
        self.root.rename(name, &self.root, "generation")?;
        directory::synchronize(&self.root)
    }

    pub(super) fn generation(&self, control: &StorageReadControl) -> BackupResult<[u8; 16]> {
        self.read_generation(control).map(|(mutation, _)| mutation)
    }

    pub(super) fn removal_generation(
        &self,
        control: &StorageReadControl,
    ) -> BackupResult<[u8; 16]> {
        self.read_generation(control).map(|(_, removal)| removal)
    }

    fn read_generation(&self, control: &StorageReadControl) -> BackupResult<([u8; 16], [u8; 16])> {
        let mut input = directory::open_file(&self.root, "generation", false)?;
        let mut bytes = [0; 24];
        read_exact(&mut input, &mut bytes, control)?;
        let mutation = ObjectId::from_bytes(bytes[8..].try_into().unwrap())?.as_bytes();
        let removal = match &bytes[..8] {
            b"UQAGEN01" => mutation,
            b"UQAGEN02" => {
                let mut removal = [0; 16];
                read_exact(&mut input, &mut removal, control)?;
                ObjectId::from_bytes(removal)?.as_bytes()
            }
            _ => return Err(BackupError::Invalid("repository generation")),
        };
        require_end(&mut input, control)?;
        Ok((mutation, removal))
    }
}
