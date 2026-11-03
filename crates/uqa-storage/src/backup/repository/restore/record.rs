//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io;

use cap_std::fs::Dir;
use uqa_core::memory::BudgetedVec;

use crate::backup::frame::require_end;
use crate::backup::{
    read_frame, write_frame, BackupError, BackupResult, FrameContext, FrameKind, ObjectId,
    RestoreId,
};
use crate::read_control::StorageReadControl;

use super::super::{directory, LocalRepository};

fn context(repository: &LocalRepository, id: RestoreId, kind: FrameKind) -> FrameContext {
    FrameContext {
        repository: repository.id(),
        object: ObjectId::from_bytes(id.as_bytes()).expect("nonzero restore identity"),
        offset: 0,
        kind,
    }
}

pub(super) fn read(
    repository: &LocalRepository,
    directory: &Dir,
    id: RestoreId,
    name: &str,
    kind: FrameKind,
    control: &StorageReadControl,
) -> BackupResult<Option<BudgetedVec<u8>>> {
    let mut file = match directory::open_file(directory, name, false) {
        Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let payload = read_frame(
        &mut file,
        context(repository, id, kind),
        &repository.protection,
        repository.limits,
        control,
    )?;
    require_end(&mut file, control)?;
    Ok(Some(payload))
}

pub(super) fn matches(
    repository: &LocalRepository,
    directory: &Dir,
    id: RestoreId,
    name: &str,
    kind: FrameKind,
    expected: &[u8],
    control: &StorageReadControl,
) -> BackupResult<bool> {
    let Some(bytes) = read(repository, directory, id, name, kind, control)? else {
        return Ok(false);
    };
    if *bytes != *expected {
        return Err(BackupError::Integrity);
    }
    Ok(true)
}

/// Publish only a completely written and synchronized immutable record. An interrupted temporary write cannot occupy the final name or prevent a later resolution attempt.
pub(super) fn install(
    repository: &LocalRepository,
    directory: &Dir,
    id: RestoreId,
    name: &str,
    kind: FrameKind,
    payload: &[u8],
    control: &StorageReadControl,
) -> BackupResult<()> {
    if matches(repository, directory, id, name, kind, payload, control)? {
        directory::open_file(directory, name, true)?.sync_all()?;
        return directory::synchronize(directory);
    }
    let temporary = directory::name(ObjectId::new()?.as_bytes());
    let temporary = directory::text(&temporary);
    let mut file = directory::create_file(directory, temporary)?;
    let result = (|| {
        write_frame(
            &mut file,
            context(repository, id, kind),
            &repository.protection,
            payload,
            repository.limits,
            control,
        )?;
        file.sync_all()?;
        match directory.hard_link(temporary, directory, name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !matches(repository, directory, id, name, kind, payload, control)? {
                    return Err(BackupError::Integrity);
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })();
    drop(file);
    let removed = directory.remove_file(temporary);
    result?;
    removed?;
    #[cfg(test)]
    repository.before(super::super::Fault::RestoreRecordSync)?;
    directory::synchronize(directory)
}
