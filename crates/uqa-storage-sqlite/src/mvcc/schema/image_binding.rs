//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The immutable image binding remains part of the restored physical format.

use rusqlite::{params, Connection};
use uqa_storage::{
    backup::BackupRoot,
    mvcc::{DatabaseRestore, VersionError},
    read_control::StorageReadControl,
};

use super::PhysicalResult;

pub(in crate::mvcc) const IMAGE: (&str, &str) = ("_uqa_backup_import", "CREATE TABLE _uqa_backup_import (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), root BLOB NOT NULL CHECK(typeof(root) = 'blob' AND length(root) = 240), target BLOB NOT NULL CHECK(typeof(target) = 'blob' AND length(target) = 16))");

pub(in crate::mvcc) fn validate(connection: &Connection) -> PhysicalResult<()> {
    match super::definition_matches(connection, IMAGE.0, IMAGE.1)? {
        None => return Ok(()),
        Some(true) => (),
        Some(false) => {
            return Err(VersionError::InvalidEncoding("changed image binding schema").into())
        }
    }
    for event in ["INSERT", "UPDATE", "DELETE"] {
        let (name, sql) = super::trigger(IMAGE.0, event);
        if super::definition_matches(connection, &name, &sql)? != Some(true) {
            return Err(VersionError::InvalidEncoding("missing image binding guard").into());
        }
    }
    Ok(())
}

/// Match the exact immutable archive before the restore owner changes either physical history.
pub(in crate::mvcc) fn validate_image(
    connection: &Connection,
    root: BackupRoot,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    control.check().map_err(VersionError::from)?;
    validate(connection)?;
    let _bindings = control
        .memory()
        .reserve(BackupRoot::ENCODED_BYTES + 16)
        .map_err(VersionError::from)?;
    let matches: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_backup_import WHERE singleton = 1 AND root = ?1 AND target = ?2)", params![root.encode().as_slice(), request.target().as_bytes().as_slice()], |row| row.get(0))?;
    if request.source() != root.cut().source || !matches {
        return Err(VersionError::InvalidEncoding("image recovery binding mismatch").into());
    }
    Ok(())
}
