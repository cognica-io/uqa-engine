//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical DML row-lock acquisition and committed-chain following.

use std::collections::BTreeSet;
use std::sync::Arc;

use uqa_core::DocId;
use uqa_sql::SQLError;

use super::{MutationLockTarget, PhysicalDocumentIdentity, PhysicalMutationLockTarget};
use crate::Engine;

pub(in crate::sql) struct MutationLockCleanup {
    manager: Arc<crate::row_locks::RowLockManager>,
    acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
}

impl MutationLockCleanup {
    pub(in crate::sql) fn new(engine: &Engine) -> Self {
        Self {
            manager: Arc::clone(&engine.row_locks),
            acquisitions: Vec::new(),
        }
    }

    pub(in crate::sql) fn acquire(
        &mut self,
        engine: &Engine,
        table: &str,
        display_name: &str,
        doc_id: DocId,
        strength: uqa_sql::ast::LockStrength,
    ) -> Result<bool, SQLError> {
        match engine.lock_row(
            table,
            doc_id,
            strength,
            uqa_sql::ast::LockWait::Block,
            display_name,
        )? {
            crate::row_locks::LockAcquire::Granted {
                acquisition,
                waited,
                ..
            } => {
                self.acquisitions.extend(acquisition);
                Ok(waited)
            }
            crate::row_locks::LockAcquire::Skipped => Err(SQLError::Internal(
                "blocking mutation lock unexpectedly skipped a row".into(),
            )),
        }
    }

    pub(in crate::sql) fn retain(
        &mut self,
        acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
    ) {
        self.acquisitions.extend(acquisitions);
    }

    pub(in crate::sql) fn rollback(&self, acquisitions: Vec<crate::row_locks::RowLockAcquisition>) {
        for acquisition in acquisitions.into_iter().rev() {
            self.manager.rollback_acquisition(acquisition);
        }
    }
}

impl Drop for MutationLockCleanup {
    fn drop(&mut self) {
        for acquisition in self.acquisitions.drain(..).rev() {
            self.manager.rollback_acquisition(acquisition);
        }
    }
}

pub(in crate::sql) fn concurrent_update_serialization_failure() -> SQLError {
    SQLError::Routine {
        sqlstate: "40001".into(),
        message: "could not serialize access due to concurrent update".into(),
    }
}

pub(in crate::sql) fn lock_mutation_row(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<bool, SQLError> {
    match engine.lock_row(
        table,
        doc_id,
        strength,
        uqa_sql::ast::LockWait::Block,
        display_name,
    )? {
        crate::row_locks::LockAcquire::Granted { waited, .. } => Ok(waited),
        crate::row_locks::LockAcquire::Skipped => Err(SQLError::Internal(
            "DML row locking used SKIP LOCKED".into(),
        )),
    }
}

/// Lock a DML target row and follow any primary-key rewrite another transaction committed while this statement waited, exactly like `PostgreSQL` 18 follows the update chain to the row version it lands on. Returns the doc id the statement must act on together with whether any wait or successor hop makes a re-qualification necessary. Callers acquire every row dependency first and promote the backend writer only after that lock phase has completed.
pub(in crate::sql) fn lock_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<MutationLockTarget, SQLError> {
    let mut current = doc_id;
    let mut recheck = false;
    let mut hops = 0usize;
    loop {
        recheck |= lock_mutation_row(engine, table, display_name, current, strength)?;
        let successor = match engine.committed_row_successor(table, current)? {
            crate::row_locks::RowChangeTarget::Unchanged => {
                return Ok(MutationLockTarget::Present {
                    doc_id: current,
                    recheck,
                });
            }
            crate::row_locks::RowChangeTarget::Deleted
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Deleted => {
                return Ok(MutationLockTarget::Deleted);
            }
            crate::row_locks::RowChangeTarget::Present(_)
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Present(successor) => successor,
        };
        if successor == current {
            return Ok(MutationLockTarget::Present {
                doc_id: current,
                recheck: true,
            });
        }
        hops += 1;
        if hops > 64 {
            return Err(SQLError::Internal(format!(
                "primary-key rewrite chain for `{table}` row {doc_id} did not converge"
            )));
        }
        recheck = true;
        current = successor;
    }
}

/// Lock a DML candidate and follow a committed update chain across physical relations. Declarative partition movement changes the leaf table as well as the document id, so callers that scan a hierarchy must retain the complete successor identity.
pub(in crate::sql) fn lock_physical_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<PhysicalMutationLockTarget, SQLError> {
    let mut current = PhysicalDocumentIdentity {
        table: table.to_string(),
        doc_id,
    };
    let mut recheck = false;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "physical rewrite chain for `{display_name}` row {table}:{doc_id} contains a cycle at {}:{}",
                current.table, current.doc_id
            )));
        }
        recheck |= lock_mutation_row(
            engine,
            &current.table,
            display_name,
            current.doc_id,
            strength,
        )?;
        match engine.committed_physical_row_successor(&current.table, current.doc_id)? {
            crate::row_locks::PhysicalRowChangeTarget::Unchanged => {
                return Ok(PhysicalMutationLockTarget::Present {
                    identity: current,
                    recheck,
                });
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted
            | crate::row_locks::PhysicalRowChangeTarget::Present { .. }
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted => {
                return Ok(PhysicalMutationLockTarget::Deleted);
            }
            crate::row_locks::PhysicalRowChangeTarget::Present { table_hash, doc_id } => {
                let table = engine.row_lock_table_for_hash(table_hash)?;
                if table == current.table && doc_id == current.doc_id {
                    return Ok(PhysicalMutationLockTarget::Present {
                        identity: current,
                        recheck: true,
                    });
                }
                recheck = true;
                current = PhysicalDocumentIdentity { table, doc_id };
            }
        }
    }
}
