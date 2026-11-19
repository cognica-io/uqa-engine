//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ffi::OsString;
use std::io;
use std::path::Path;

use cap_std::fs::Dir;
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryReservation;

use crate::backup::{BackupError, BackupResult, RestoreId};
use crate::native_file::directory as native;
use crate::read_control::StorageReadControl;

use super::super::directory;

pub(super) struct Destination {
    pub parent: Dir,
    pub name: OsString,
    _charge: MemoryReservation,
}

impl Destination {
    pub fn open(path: &Path, control: &StorageReadControl) -> BackupResult<Self> {
        let (parent, name) = directory::parent(path)?;
        native::component(name)?;
        let mut charge = control.memory().reserve(name.len())?;
        let name = name.to_os_string();
        charge.grow(name.capacity().saturating_sub(charge.bytes()))?;
        Ok(Self {
            parent,
            name,
            _charge: charge,
        })
    }

    pub fn name_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"UQA restore destination component v1\0");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            digest.update(self.name.as_bytes());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            for character in self.name.encode_wide() {
                digest.update(character.to_le_bytes());
            }
        }
        digest.finalize().into()
    }

    pub fn require_absent(&self) -> BackupResult<()> {
        match self.parent.symlink_metadata(&self.name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(occupied()),
        }
    }
}

pub(super) fn holder_name(id: RestoreId) -> [u8; 45] {
    let mut name = [0; 45];
    name[..13].copy_from_slice(b".uqa-restore-");
    name[13..].copy_from_slice(&directory::name(id.as_bytes()));
    name
}

pub(super) fn holder(destination: &Destination, id: RestoreId) -> BackupResult<Dir> {
    let name = holder_name(id);
    directory::child(
        &destination.parent,
        std::str::from_utf8(&name).unwrap().as_ref(),
    )
}

pub(super) fn occupied() -> BackupError {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "restore destination already exists",
    )
    .into()
}
