//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private image import and recovery hold exclusive restore admission until every physical connection closes.

use std::{path::Path, sync::Arc};

use rusqlite::Connection;
use uqa_storage::{
    backup::BackupRoot, mvcc::DatabaseRestore, read_control::StorageReadControl,
    StorageEncryptionKey,
};

use super::{
    compressed_vfs,
    ownership::{DatabaseOwner, RestoreAdmission},
    source::DatabaseSource,
    ConnectionPool, ConnectionSpec, ManagedConnection, Result, SQLiteCompressionOptions,
    SQLiteError, SessionState,
};

pub(crate) struct ImageConnection {
    connection: Connection,
    spec: ConnectionSpec,
    source: Option<DatabaseSource>,
    owner: Arc<DatabaseOwner>,
    admission: RestoreAdmission,
}

impl ImageConnection {
    pub(crate) fn create(
        path: &Path,
        compression: Option<SQLiteCompressionOptions>,
        key: Option<StorageEncryptionKey>,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::open(path, compression, key, control, true)
    }

    pub(crate) fn reopen(
        path: &Path,
        compression: Option<SQLiteCompressionOptions>,
        key: Option<StorageEncryptionKey>,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::open(path, compression, key, control, false)
    }

    fn open(
        path: &Path,
        compression: Option<SQLiteCompressionOptions>,
        key: Option<StorageEncryptionKey>,
        control: &StorageReadControl,
        fresh: bool,
    ) -> Result<Self> {
        control.check()?;
        if key
            .as_ref()
            .is_some_and(|key| key.expose_secret().is_empty())
        {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        let compression = compression
            .map(SQLiteCompressionOptions::validate)
            .transpose()
            .map_err(SQLiteError::CompressedContainer)?;
        let admission = RestoreAdmission::acquire(path, control)?;
        let metadata = path.symlink_metadata()?;
        if !metadata.is_file() || (metadata.len() == 0) != fresh {
            return Err(SQLiteError::StorageBackend(
                if fresh {
                    "image import requires an empty private regular file"
                } else {
                    "image recovery requires a populated private regular file"
                }
                .into(),
            ));
        }
        let owner = admission.retain(control)?;
        let path = path.canonicalize()?;
        let mut spec = if let Some(compression) = compression {
            compressed_vfs::register_database(
                &path,
                compression,
                key.as_ref().map(StorageEncryptionKey::expose_secret),
            )
            .map_err(SQLiteError::CompressedContainer)?;
            ConnectionSpec::Compressed {
                path,
                compression,
                key,
            }
        } else {
            ConnectionSpec::File { path, key }
        };
        let mode = if fresh {
            super::ConnectionOpen::Initial
        } else {
            super::ConnectionOpen::Restored
        };
        let connection = spec.open(mode)?;
        let source = DatabaseSource::capture(&mut spec, &connection)?;
        Ok(Self {
            connection,
            spec,
            source,
            owner,
            admission,
        })
    }

    pub(crate) fn physical(&self) -> Result<&Connection> {
        if let Some(source) = &self.source {
            source.check()?;
            source.check_connection(&self.connection)?;
        }
        Ok(&self.connection)
    }

    pub(crate) fn publish_history(
        self,
        root: BackupRoot,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<()> {
        self.physical()?;
        let Self {
            connection,
            spec,
            source,
            owner,
            admission,
        } = self;
        let result = (|| {
            let connection = ManagedConnection {
                pool: ConnectionPool::new(spec, connection, 1, Some(owner))?,
                session: Arc::new(SessionState::new()),
                record_access: false,
            };
            crate::mvcc::restore::publish_image(&connection, root, request, control)?;
            Ok(())
        })();
        drop(source);
        drop(admission);
        result
    }
}
