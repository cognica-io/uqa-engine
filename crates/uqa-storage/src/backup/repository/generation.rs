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
    /// Called during exclusive repository mutation or initial creation. Each replacement changes the comparison identity before installing new references; collectors must revalidate it under the same admission before deleting candidates.
    pub(super) fn advance_generation(&self, control: &StorageReadControl) -> BackupResult<()> {
        let generation = ObjectId::new()?.as_bytes();
        let mut name = [0; 44];
        name[..12].copy_from_slice(b".generation-");
        name[12..].copy_from_slice(&directory::name(generation));
        let name = std::str::from_utf8(&name).expect("fixed repository generation name");
        let mut pending = directory::create_file(&self.root, name)?;
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(b"UQAGEN01");
        bytes[8..].copy_from_slice(&generation);
        // This bounded local coordination record contains no source data. Its atomic
        // replacement is not a replay-protection claim against repository rewriting.
        write_all(&mut pending, &bytes, control)?;
        pending.sync_all()?;
        self.root.rename(name, &self.root, "generation")?;
        directory::synchronize(&self.root)
    }

    pub(super) fn generation(&self, control: &StorageReadControl) -> BackupResult<[u8; 16]> {
        let mut input = directory::open_file(&self.root, "generation", false)?;
        let mut bytes = [0; 24];
        read_exact(&mut input, &mut bytes, control)?;
        require_end(&mut input, control)?;
        if &bytes[..8] != b"UQAGEN01" {
            return Err(BackupError::Invalid("repository generation"));
        }
        Ok(ObjectId::from_bytes(bytes[8..].try_into().unwrap())?.as_bytes())
    }
}
