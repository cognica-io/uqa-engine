//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed-backup constructors retain exclusive physical ownership through history publication.

use std::{path::Path, sync::Arc};

use uqa_storage::{mvcc::DatabaseRestore, read_control::StorageReadControl, StorageEncryptionKey};

use super::{
    compressed_vfs, default_pool_connections, ownership::RestoreAdmission, ConnectionPool,
    ConnectionSpec, ManagedConnection, Result, SQLiteCompressedContainerAnchor,
    SQLiteCompressionOptions, SQLiteError, SessionState,
};

impl ManagedConnection {
    /// Restore an existing, closed consistent backup into the request's new transaction-history incarnation. Persist the request outside the database first; retry that same request after any error. Intermediate durable states reject ordinary opens. All connections, sessions, snapshots and SSI participants must be released first. A completed retry preserves later work. The returned connection can be bound to its original native or Key/Value mapping.
    pub fn open_restored(
        path: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::restore_with_spec(path, request, control, || {
            Ok(ConnectionSpec::File {
                path: path.to_path_buf(),
                key: None,
            })
        })
    }

    /// `SQLCipher` counterpart of [`Self::open_restored`], using the same credential for its SSI database.
    pub fn open_encrypted_restored(
        path: &Path,
        key: &str,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        if key.is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        Self::restore_with_spec(path, request, control, || {
            Ok(ConnectionSpec::File {
                path: path.to_path_buf(),
                key: Some(StorageEncryptionKey::new(key)),
            })
        })
    }

    /// Compressed-container counterpart of [`Self::open_restored`].
    pub fn open_compressed_restored(
        path: &Path,
        compression: SQLiteCompressionOptions,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::restore_compressed(path, compression, None, None, request, control)
    }

    /// Encrypted compressed-container counterpart of [`Self::open_restored`].
    pub fn open_compressed_encrypted_restored(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::restore_compressed(path, compression, Some(key), None, request, control)
    }

    /// Enforce the caller's trusted current container anchor before restoring. As with ordinary anchored opening, retain an updated anchor after a successful write; an old anchor cannot authorize a retry against a changed container.
    pub fn open_compressed_encrypted_restored_with_anchor(
        path: &Path,
        key: &str,
        compression: SQLiteCompressionOptions,
        trusted_anchor: SQLiteCompressedContainerAnchor,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Self::restore_compressed(
            path,
            compression,
            Some(key),
            Some(trusted_anchor),
            request,
            control,
        )
    }

    fn restore_compressed(
        path: &Path,
        compression: SQLiteCompressionOptions,
        key: Option<&str>,
        anchor: Option<SQLiteCompressedContainerAnchor>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> Result<Self> {
        if key == Some("") {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        let compression = compression
            .validate()
            .map_err(SQLiteError::CompressedContainer)?;
        Self::restore_with_spec(path, request, control, || {
            match anchor {
                Some(anchor) => compressed_vfs::register_database_with_anchor(
                    path,
                    compression,
                    key.ok_or(SQLiteError::EncryptionKeyRequired)?,
                    anchor,
                ),
                None => compressed_vfs::register_database(path, compression, key),
            }
            .map_err(SQLiteError::CompressedContainer)?;
            Ok(ConnectionSpec::Compressed {
                path: path.to_path_buf(),
                compression,
                key: key.map(StorageEncryptionKey::new),
            })
        })
    }

    fn restore_with_spec(
        path: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
        spec: impl FnOnce() -> Result<ConnectionSpec>,
    ) -> Result<Self> {
        control.check()?;
        let admission = RestoreAdmission::acquire(path, control)?;
        if path.metadata()?.len() == 0 {
            return Err(SQLiteError::StorageBackend(
                "database restoration requires an initialized record history".into(),
            ));
        }
        // Allocate the returned owner before any durable transition, and install it under exclusive admission.
        let owner = admission.retain(control)?;
        let spec = spec()?;
        let initial = spec.open(false)?;
        let connection = Self {
            pool: ConnectionPool::new(spec, initial, default_pool_connections(), Some(owner)),
            session: Arc::new(SessionState::new()),
            record_access: false,
        };
        crate::mvcc::restore::publish(&connection, request, control)?;
        Ok(connection)
    }
}
