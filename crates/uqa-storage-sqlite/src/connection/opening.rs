//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial, restored, and retained physical connection admission.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use uqa_storage::StorageEncryptionKey;

use super::{
    compressed_vfs, ManagedConnection, Result, SQLiteCompressionOptions, SQLiteError,
    PREPARED_STATEMENT_CACHE_CAPACITY,
};

#[derive(Clone)]
pub(super) enum ConnectionSpec {
    File {
        path: PathBuf,
        key: Option<super::encryption::FileKey>,
    },
    Auxiliary {
        path: PathBuf,
        key: Option<super::encryption::FileKey>,
    },
    Compressed {
        path: PathBuf,
        compression: SQLiteCompressionOptions,
        key: Option<StorageEncryptionKey>,
        trusted_anchor: Option<super::SQLiteCompressedContainerAnchor>,
    },
    Memory,
}

#[derive(Clone, Copy)]
pub(super) enum ConnectionOpen {
    Initial,
    Restored,
    Pooled,
}

impl ConnectionSpec {
    pub(super) fn open_initial(&self) -> Result<Connection> {
        let header = match self {
            Self::File { path, key: Some(_) }
            | Self::Auxiliary { path, key: Some(_) }
            | Self::Compressed {
                path, key: Some(_), ..
            } if !path.as_os_str().is_empty() && path != std::path::Path::new(":memory:") => {
                super::ownership::admit_file_header(
                    path,
                    &uqa_storage::read_control::StorageReadControl::with_limit(
                        uqa_storage::mvcc::VersionedSessionOptions::default().retained_bytes,
                    ),
                )?
            }
            _ => None,
        };
        let mut connection = self.open(ConnectionOpen::Initial)?;
        if header.is_some() && matches!(self, Self::Auxiliary { .. } | Self::Compressed { .. }) {
            // WAL initialization writes the main header. A new DELETE-mode file
            // needs an explicit header write before another connection chooses its salt
            // or a compressed pool derives credentials from its container identity.
            connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?
                .commit()?;
        }
        Ok(connection)
    }

    pub(super) fn open(&self, mode: ConnectionOpen) -> Result<Connection> {
        let conn = self.open_configured(mode)?;
        conn.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE_CAPACITY);
        Ok(conn)
    }

    fn open_configured(&self, mode: ConnectionOpen) -> Result<Connection> {
        let initialize_database = matches!(mode, ConnectionOpen::Initial);
        let mut flags = OpenFlags::default();
        if !initialize_database && matches!(self, Self::File { .. } | Self::Auxiliary { .. }) {
            flags.remove(OpenFlags::SQLITE_OPEN_CREATE);
        }
        match self {
            Self::File { path, key } | Self::Auxiliary { path, key } => {
                let conn = Connection::open_with_flags(path, flags)?;
                if let Some(key) = key {
                    key.apply(&conn)?;
                }
                crate::btree_index::equality::register(&conn)?;
                if matches!(self, Self::Auxiliary { .. }) {
                    // Registry writers already serialize their transactions.
                    // Retain the original journal mode instead of racing to
                    // promote a new registry to WAL during concurrent opens.
                    let mode: String =
                        conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
                    if mode != "delete" {
                        return Err(SQLiteError::AuxiliaryJournalMode(mode));
                    }
                    ManagedConnection::configure_rollback_connection(&conn)?;
                } else {
                    if initialize_database {
                        ManagedConnection::enable_wal(&conn)?;
                    }
                    ManagedConnection::configure_wal_connection(&conn)?;
                }
                Ok(conn)
            }
            Self::Compressed {
                path,
                compression,
                key,
                trusted_anchor,
            } => {
                let vfs = if matches!(mode, ConnectionOpen::Pooled) {
                    compressed_vfs::POOL_VFS_NAME
                } else {
                    let key = key.as_ref().map(StorageEncryptionKey::expose_secret);
                    match trusted_anchor {
                        Some(anchor) => compressed_vfs::register_database_with_anchor(
                            path,
                            *compression,
                            key.ok_or(SQLiteError::EncryptionKeyRequired)?,
                            *anchor,
                        ),
                        None => compressed_vfs::register_database(path, *compression, key),
                    }
                    .map_err(SQLiteError::CompressedContainer)?;
                    compressed_vfs::VFS_NAME
                };
                let conn = Connection::open_with_flags_and_vfs(path, flags, vfs)?;
                crate::btree_index::equality::register(&conn)?;
                if initialize_database {
                    conn.pragma_update(None, "page_size", compression.page_size)?;
                    ManagedConnection::enable_compressed_journal(&conn)?;
                }
                ManagedConnection::configure_rollback_connection(&conn)?;
                Ok(conn)
            }
            Self::Memory => {
                let conn = Connection::open_in_memory()?;
                crate::btree_index::equality::register(&conn)?;
                if initialize_database {
                    ManagedConnection::enable_wal(&conn)?;
                }
                ManagedConnection::configure_wal_connection(&conn)?;
                Ok(conn)
            }
        }
    }
}
