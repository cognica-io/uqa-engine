//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind execution lock requests to the active engine transaction.

use crate::Engine;
use std::sync::Arc;
use uqa_core::DocId;
use uqa_execution::row_locks::{
    session::RowLockSession, LockAcquire, PhysicalRowChangeTarget, RowChangeTarget, RowLockManager,
};
use uqa_sql::{
    ast::{LockStrength, LockWait},
    SQLError,
};

impl RowLockSession for Engine {
    fn lock_manager(&self) -> Arc<RowLockManager> {
        self.row_lock_manager()
    }
    fn lock_row(
        &self,
        table: &str,
        doc_id: DocId,
        strength: LockStrength,
        wait: LockWait,
        display_name: &str,
    ) -> Result<LockAcquire, SQLError> {
        Engine::lock_row(self, table, doc_id, strength, wait, display_name)
    }
    fn uses_fixed_snapshot(&self) -> bool {
        self.current_transaction_uses_fixed_snapshot()
    }
    fn committed_row_successor(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<RowChangeTarget, SQLError> {
        Engine::committed_row_successor(self, table, doc_id)
    }
    fn committed_physical_row_successor(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<PhysicalRowChangeTarget, SQLError> {
        Engine::committed_physical_row_successor(self, table, doc_id)
    }
    fn table_for_lock_hash(&self, table_hash: u64) -> Result<String, SQLError> {
        self.row_lock_table_for_hash(table_hash)
    }
}
