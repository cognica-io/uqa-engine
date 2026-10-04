//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The index of the rows a transaction changed, which reads at a fixed snapshot take views of instead of walking every row change of the transaction. The outer frame owns it; it follows the frames' row changes from the first read that needs it, and an index that cannot follow a rollback is dropped and built again from the row changes when a read next needs it.

use super::{Engine, SQLError, StorageSavepointId, TransactionFrame, TransactionRowChange};
use crate::row_locks::PendingRowChangeKind;
use uqa_execution::query::document_changes::{
    ChangedIdentities, ChangedIdentitiesView, RelationGeneration,
};
use uqa_execution::storage_errors::storage_error;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::StorageBackendResult;

/// Record `change` in `identities`: the row it changed, and for a key rewrite the row it moved to.
fn note(
    identities: &mut ChangedIdentities,
    change: &TransactionRowChange,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let id = change.pending.key.doc_id;
    // A first change records whether the row was present before the transaction changed it.
    match change.pending.kind {
        PendingRowChangeKind::Insert => {
            identities.note(&change.source_generation, id, false, true, control)
        }
        PendingRowChangeKind::Update => {
            identities.note(&change.source_generation, id, true, true, control)
        }
        PendingRowChangeKind::Delete => {
            identities.note(&change.source_generation, id, true, false, control)
        }
        PendingRowChangeKind::Rewrite(successor) => {
            identities.note(&change.source_generation, id, true, false, control)?;
            match &change.successor_generation {
                Some(generation) => {
                    identities.note(generation, successor.doc_id, false, true, control)
                }
                None => Ok(()),
            }
        }
    }
}

/// Apply `operation` to the outer frame's index, dropping the index when it cannot follow: it is built again from the row changes when a read needs it.
pub(super) fn follow_identities(
    stack: &mut [TransactionFrame],
    operation: impl FnOnce(&mut ChangedIdentities) -> StorageBackendResult<bool>,
) {
    let Some(outer) = stack.first_mut() else {
        return;
    };
    if let Some(identities) = outer.fixed_identities.as_mut() {
        if !operation(identities).unwrap_or(false) {
            outer.fixed_identities = None;
        }
    }
}

/// Mark savepoint `id` in the outer frame's index.
pub(super) fn save_identities(stack: &mut [TransactionFrame], id: StorageSavepointId) {
    follow_identities(stack, |identities| identities.savepoint(id).map(|()| true));
}

/// Return the outer frame's index to savepoint `id` and release the savepoint, as the rollback of a nested frame does.
pub(super) fn rollback_identities(stack: &mut [TransactionFrame], id: StorageSavepointId) {
    follow_identities(stack, |identities| {
        Ok(identities.rollback_to(id)? && identities.release(id)?)
    });
}

impl Engine {
    /// Record a row change the newest frame just took in the outer frame's index, if it keeps one.
    pub(super) fn follow_row_change(
        &self,
        stack: &mut [TransactionFrame],
        change: &TransactionRowChange,
    ) -> Result<(), SQLError> {
        if stack
            .first()
            .is_none_or(|outer| outer.fixed_identities.is_none())
        {
            return Ok(());
        }
        let control = self.query_retention_control()?;
        follow_identities(stack, |identities| {
            note(identities, change, &control).map(|()| true)
        });
        Ok(())
    }

    /// A view of the rows the transaction changed in relation generation `generation`, building the outer frame's index from the frames' row changes when it keeps none; `None` when the transaction changed no row of it or has no frame.
    pub(crate) fn fixed_identities_view(
        &self,
        generation: &RelationGeneration,
    ) -> Result<Option<ChangedIdentitiesView>, SQLError> {
        let control = self.query_retention_control()?;
        let mut stack = self.session.transactions.lock();
        if stack.is_empty() {
            return Ok(None);
        }
        if stack[0].fixed_identities.is_none() {
            let mut identities = ChangedIdentities::new(&control)
                .map_err(|error| storage_error("index transaction row changes", &error))?;
            for change in stack.iter().flat_map(|frame| frame.row_changes.iter()) {
                note(&mut identities, change, &control)
                    .map_err(|error| storage_error("index transaction row changes", &error))?;
            }
            // Savepoints marked before the index existed are unknown to it; rolling back to one drops the index.
            stack[0].fixed_identities = Some(identities);
        }
        stack[0]
            .fixed_identities
            .as_mut()
            .expect("an indexed transaction")
            .view(generation)
            .map_err(|error| storage_error("view transaction row changes", &error))
    }
}
