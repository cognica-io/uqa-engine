//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable read data shared by the commands and query of one SQL statement.

use super::{
    Engine, SQLError, SessionPortalCatalogSnapshot, SessionPortalTableDependencies,
    SessionPortalTableSnapshots,
};

/// Frozen table and catalog handles with no session, transaction, or mutation capability.
#[derive(Clone)]
pub(crate) struct StatementReadSnapshot {
    tables: SessionPortalTableSnapshots,
    catalog: SessionPortalCatalogSnapshot,
    transaction_origin: u64,
}

impl Engine {
    /// Capture the relation and catalog state visible at the start of a SQL statement. A `BEFORE STATEMENT` trigger executes inside the statement's transaction and may therefore change the live engine before the statement evaluates its source query. `PostgreSQL` keeps those changes outside the statement snapshot, so the remaining query work must read through an immutable query engine while trigger and row effects continue to use the live engine.
    pub(crate) fn capture_statement_read_snapshot(
        &self,
    ) -> Result<StatementReadSnapshot, SQLError> {
        let dependencies = SessionPortalTableDependencies::all();
        let snapshot_gate = self
            .row_locks
            .begin_change_snapshot(&self.runtime.cancellation)?;
        let transaction_overlay = self.capture_session_portal_transaction_overlay()?;
        snapshot_gate.baseline()?;
        drop(snapshot_gate);
        let table_sources = {
            let stack = self.session.transactions.lock();
            let fixed_snapshot = stack
                .first()
                .and_then(|frame| frame.fixed_snapshot.as_ref());
            self.capture_session_portal_table_sources(fixed_snapshot, &dependencies)
        };
        let table_snapshots = Self::detach_session_portal_table_snapshots(
            table_sources,
            transaction_overlay.as_ref(),
        )?;
        let mut catalog_snapshot = self.durable.snapshot();
        catalog_snapshot.graphs = self.freeze_graph_read_handles(None, true)?;
        let catalog_snapshot = std::sync::Arc::new(catalog_snapshot);
        Ok(StatementReadSnapshot {
            tables: table_snapshots,
            catalog: catalog_snapshot,
            transaction_origin: self.allocate_session_portal_transaction_origin(),
        })
    }

    pub(crate) fn statement_read_snapshot_engine(
        &self,
        snapshot: &StatementReadSnapshot,
    ) -> Engine {
        self.session_portal_worker_engine(
            std::sync::Arc::clone(&snapshot.tables),
            std::sync::Arc::clone(&snapshot.catalog.views),
            std::sync::Arc::clone(&snapshot.catalog.sql_user_functions),
            std::sync::Arc::clone(&snapshot.catalog),
            snapshot.transaction_origin,
        )
    }
}
