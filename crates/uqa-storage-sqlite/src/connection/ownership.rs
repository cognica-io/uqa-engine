//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical database owners exclude restoration across pools and retained resources.

use std::{path::Path, sync::Arc};

use uqa_core::memory::MemoryReservation;
use uqa_storage::{read_control::StorageReadControl, PersistentStorageIdentity};

use super::{Result, SQLiteError};

#[cfg(any(test, not(any(windows, all(unix, not(target_os = "emscripten"))))))]
mod local;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod native;
#[cfg(test)]
mod tests;

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
use local::Admission;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use native::Admission;

pub(crate) struct DatabaseOwner {
    _lease: Box<dyn Send + Sync>,
    _memory: MemoryReservation,
}

pub(super) struct RestoreAdmission(Admission);

fn admission(path: &Path, exclusive: bool, control: &StorageReadControl) -> Result<Admission> {
    control.check()?;
    let PersistentStorageIdentity::File(path) = PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("database file identity")
    };
    Admission::acquire(&path, exclusive, control)
}

fn retain(admission: &Admission, control: &StorageReadControl) -> Result<Arc<DatabaseOwner>> {
    let memory = control
        .memory()
        .reserve(std::mem::size_of::<DatabaseOwner>() + 2 * std::mem::size_of::<usize>())?;
    let lease = admission.retain(control)?;
    Ok(Arc::new(DatabaseOwner {
        _lease: lease,
        _memory: memory,
    }))
}

pub(super) fn open(path: &Path, control: &StorageReadControl) -> Result<Arc<DatabaseOwner>> {
    retain(&admission(path, false, control)?, control)
}

impl RestoreAdmission {
    pub(super) fn acquire(path: &Path, control: &StorageReadControl) -> Result<Self> {
        if !path.metadata()?.is_file() {
            return Err(SQLiteError::StorageBackend(
                "database restoration requires an existing file".into(),
            ));
        }
        Ok(Self(admission(path, true, control)?))
    }

    /// Install the returned pool's ordinary lease before opening restore admission to another owner.
    pub(super) fn retain(&self, control: &StorageReadControl) -> Result<Arc<DatabaseOwner>> {
        retain(&self.0, control)
    }
}
