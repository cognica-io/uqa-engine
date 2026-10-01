//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Commits that need no sync of their own.
//!
//! In WAL mode every fully synchronized commit, and every checkpoint, first syncs the whole log, so the durable state is always a prefix of the commit order. A commit made without its own sync is therefore durable no later than the next fully synchronized commit, and a power loss can discard it only together with every commit after it. A process failure loses no commit. Each use states why losing its commit that way is harmless. A rollback journal is not safe against power loss without full synchronization, so it keeps its sync.

use std::sync::Arc;

use rusqlite::Connection;

use super::connection_functions::ConnectionFunctions;
use super::schema::WritePermit;
use super::PhysicalResult;

/// Commits the connection's next transaction without its own sync in WAL mode, and restores full synchronization when dropped.
pub(super) struct RelaxedSynchronization<'a> {
    connection: &'a Connection,
    functions: Arc<ConnectionFunctions>,
}

impl<'a> RelaxedSynchronization<'a> {
    /// Relax the synchronization of `connection`, which must be outside a transaction, or return `None` when its journal needs every sync.
    pub(super) fn relax(
        connection: &'a Connection,
        permit: &WritePermit,
    ) -> PhysicalResult<Option<Self>> {
        let mode: String = connection
            .prepare_cached("PRAGMA journal_mode")?
            .query_row([], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Ok(None);
        }
        let functions = Arc::clone(permit.functions());
        functions.relax_synchronization(connection)?;
        Ok(Some(Self {
            connection,
            functions,
        }))
    }
}

impl Drop for RelaxedSynchronization<'_> {
    fn drop(&mut self) {
        // A failed restoration stays recorded, and the next write admission restores full synchronization or fails before writing.
        let _ = self.functions.require_full_synchronization(self.connection);
    }
}
