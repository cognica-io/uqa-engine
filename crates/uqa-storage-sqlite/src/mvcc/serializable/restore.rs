//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit restoration validates the old checkpoint before retiring its history.

use uqa_storage::{
    mvcc::{DatabaseId, DatabaseRestore, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::{schema, Error, SQLiteSerializableAdmission};
use crate::ManagedConnection;

impl SQLiteSerializableAdmission {
    pub(in crate::mvcc) fn for_restore(
        main: &ManagedConnection,
        request: DatabaseRestore,
        current: DatabaseId,
        pending: bool,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let auxiliary = main
            .open_serializable_connection()
            .map_err(|error| Error::from(error).into_version())?;
        Self::load(&auxiliary, control, |connection| {
            let present: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_uqa_serializable_state')",
                [],
                |row| row.get(0),
            )?;
            let identity = if present {
                let bytes: [u8; 16] = connection.query_row(
                    "SELECT database_id FROM _uqa_serializable_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )?;
                DatabaseId::from_bytes(bytes)
            } else {
                current
            };
            if identity != current && !(pending && identity == request.target()) {
                return Err(VersionError::WrongDatabase.into());
            }
            // A newer same-source checkpoint may accompany an older backup. Explicit restoration validates its structure, then retires it instead of claiming its terminal outcomes for the copied main data.
            Ok(identity)
        })
    }

    pub(in crate::mvcc) fn publish_restored(
        self,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if self.graph.database() == request.source() {
            schema::restore(&self.connection, request.target(), control)
                .map_err(Error::into_version)?;
        } else if self.graph.database() != request.target() {
            return Err(VersionError::WrongDatabase);
        }
        // A resumed target already has its persisted fresh coordinator and must not be reset a second time.
        super::retry(&self.connection, "COMMIT", false, control).map_err(Error::into_version)
    }
}
