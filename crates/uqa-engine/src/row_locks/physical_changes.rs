//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical tuple-successor tracking for rows that move between partitions.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalRowChangeTarget {
    Unchanged,
    Present { table_hash: u64, doc_id: DocId },
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalPhysicalRowChangeTarget {
    Unchanged,
    Present(RowLockKey),
    Deleted,
}

impl RowLockManager {
    /// Follow committed rewrites while retaining the physical relation as well as the document id. Partition movement changes both halves of the tuple identity, so DML rechecks must not collapse the successor back onto the source leaf.
    pub(crate) fn physical_row_successor_after(
        &self,
        table: &str,
        doc_id: DocId,
        baseline: RowChangeBaseline,
    ) -> Result<PhysicalRowChangeTarget, SQLError> {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            return coordinator
                .physical_change_target_after(
                    table_hash(table),
                    doc_id,
                    baseline.cross_sequence,
                    LockStrength::ForUpdate,
                )
                .map_err(SQLError::Internal);
        }
        Ok(
            match resolve_local_physical_change_target(
                &self.state.lock().changes,
                key,
                baseline.epoch,
                LockStrength::ForUpdate,
            ) {
                LocalPhysicalRowChangeTarget::Unchanged => PhysicalRowChangeTarget::Unchanged,
                LocalPhysicalRowChangeTarget::Deleted => PhysicalRowChangeTarget::Deleted,
                LocalPhysicalRowChangeTarget::Present(target) => PhysicalRowChangeTarget::Present {
                    table_hash: table_hash(self.table_name(target.table).as_ref()),
                    doc_id: target.doc_id,
                },
            },
        )
    }
}

pub(super) fn resolve_local_physical_change_target(
    changes: &[CommittedRowChange],
    key: RowLockKey,
    baseline: u64,
    wanted: LockStrength,
) -> LocalPhysicalRowChangeTarget {
    let mut current = key;
    let mut requires_recheck = false;
    for change in changes {
        if !epoch_is_after(change.epoch, baseline) || change.key != current {
            continue;
        }
        match change.kind {
            CommittedRowChangeKind::Update => {
                requires_recheck |= lock_strengths_conflict(change.strength, wanted);
            }
            CommittedRowChangeKind::Delete => {
                if lock_strengths_conflict(change.strength, wanted) {
                    return LocalPhysicalRowChangeTarget::Deleted;
                }
            }
            CommittedRowChangeKind::Rewrite(successor) => {
                if lock_strengths_conflict(change.strength, wanted) {
                    requires_recheck = true;
                    current = successor;
                }
            }
        }
    }
    if requires_recheck {
        LocalPhysicalRowChangeTarget::Present(current)
    } else {
        LocalPhysicalRowChangeTarget::Unchanged
    }
}
