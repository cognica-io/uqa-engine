//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction-scoped row-lock services consumed by mutation execution.

use super::{LockAcquire, PhysicalRowChangeTarget, RowChangeTarget, RowLockManager};
use std::sync::Arc;
use uqa_core::DocId;
use uqa_sql::{
    ast::{LockStrength, LockWait},
    SQLError,
};

/// Lock acquisition and committed-chain visibility within the caller's transaction.
pub trait RowLockSession {
    fn lock_manager(&self) -> Arc<RowLockManager>;
    fn lock_row(
        &self,
        table: &str,
        doc_id: DocId,
        strength: LockStrength,
        wait: LockWait,
        display_name: &str,
    ) -> Result<LockAcquire, SQLError>;
    fn uses_fixed_snapshot(&self) -> bool;
    fn committed_row_successor(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<RowChangeTarget, SQLError>;
    fn committed_physical_row_successor(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<PhysicalRowChangeTarget, SQLError>;
    fn table_for_lock_hash(&self, table_hash: u64) -> Result<String, SQLError>;
}
