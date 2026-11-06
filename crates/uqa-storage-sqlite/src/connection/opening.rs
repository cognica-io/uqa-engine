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
        key: Option<StorageEncryptionKey>,
    },
    Auxiliary {
        path: PathBuf,
        key: Option<StorageEncryptionKey>,
    },
    Compressed {
        path: PathBuf,
        compression: SQLiteCompressionOptions,
        key: Option<StorageEncryptionKey>,
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
                    ManagedConnection::apply_encryption_key(&conn, key.expose_secret())?;
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
                path, compression, ..
            } => {
                let vfs = if matches!(mode, ConnectionOpen::Pooled) {
                    compressed_vfs::POOL_VFS_NAME
                } else {
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
