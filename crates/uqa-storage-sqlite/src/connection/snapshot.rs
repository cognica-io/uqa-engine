//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical snapshot identity for caches attached to legacy managed sessions.

use super::{Arc, Connection, ManagedConnection, Result, SQLiteError};

pub(super) struct PhysicalConnection {
    pub(super) connection: Connection,
    identity: Arc<()>,
    /// The committed state at which a record store last found the database behind this connection to be its own.
    pub(super) validated_read: std::cell::Cell<Option<ValidatedRead>>,
}

impl PhysicalConnection {
    pub(super) fn new(connection: Connection) -> Self {
        Self {
            connection,
            identity: Arc::new(()),
            validated_read: std::cell::Cell::new(None),
        }
    }
}

/// One committed state of a database as one connection sees it, with the store that validated it. Every commit of another connection changes the connection's data version, and every row the connection itself changes counts in its changes, so an equal state is the same committed database.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedRead {
    /// The history identity the store expects, and its native namespace when it has one.
    pub(crate) store: ([u8; 16], Option<[u8; 16]>),
    pub(crate) data_version: i64,
    pub(crate) changes: u64,
}

/// Tokens retain their connection and rollback-branch identities, so allocator reuse cannot make an unrelated view compare equal.
#[derive(Clone)]
pub(crate) struct SnapshotIdentity {
    connection: Arc<()>,
    branch: Arc<()>,
    changes: u64,
    data_version: u64,
}

impl SnapshotIdentity {
    fn capture(connection: &Connection, identity: &Arc<()>, branch: &Arc<()>) -> Result<Self> {
        let version: i64 = connection.pragma_query_value(None, "data_version", |row| row.get(0))?;
        Ok(Self {
            connection: Arc::clone(identity),
            branch: Arc::clone(branch),
            changes: connection.total_changes(),
            data_version: u64::try_from(version)
                .map_err(|_| SQLiteError::StorageBackend("negative SQLite data version".into()))?,
        })
    }

    pub(crate) fn same_view(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.connection, &other.connection)
            && Arc::ptr_eq(&self.branch, &other.branch)
            && self.changes == other.changes
            && self.data_version == other.data_version
    }
}

impl ManagedConnection {
    /// Hold one physical snapshot across cache selection, evaluation and optional writes. The returned identity belongs to the evaluated result, never a later checkout or commit.
    pub(crate) fn with_snapshot<R>(
        &self,
        operation: impl FnOnce(&Connection, &SnapshotIdentity) -> Result<R>,
    ) -> Result<(R, SnapshotIdentity)> {
        self.with_physical_mut(|physical| {
            let transaction = physical.connection.savepoint()?;
            let _: i64 =
                transaction
                    .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))?;
            let branch = Arc::clone(&self.session.snapshot_branch.lock());
            let before = SnapshotIdentity::capture(&transaction, &physical.identity, &branch)?;
            let result = operation(&transaction, &before)?;
            let after = SnapshotIdentity::capture(&transaction, &physical.identity, &branch)?;
            transaction.commit()?;
            Ok((result, after))
        })
    }
}
