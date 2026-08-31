//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-change observation, resolution, and publication.

use super::{
    cross_process, mutation_strength, normalize_pending_row_changes, remove_inactive_versions,
    resolve_local_change_target, row_has_waiter, table_hash, Arc, CrossAttachment, DocId,
    LockStrength, RowLockKey, RowLockManager, SQLError,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowChangeBaseline {
    pub(crate) epoch: u64,
    pub(crate) cross_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingRowChange {
    pub(crate) key: RowLockKey,
    pub(crate) kind: PendingRowChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingRowChangeKind {
    Insert,
    Update,
    Delete,
    Rewrite(RowLockKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowChangeTarget {
    Unchanged,
    Present(DocId),
    Deleted,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CommittedRowChange {
    pub(super) epoch: u64,
    pub(super) key: RowLockKey,
    pub(super) kind: CommittedRowChangeKind,
    pub(super) strength: LockStrength,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CommittedRowChangeKind {
    Update,
    Delete,
    Rewrite(RowLockKey),
}

pub(crate) struct RowChangeObservation {
    manager: Arc<RowLockManager>,
}

impl Drop for RowChangeObservation {
    fn drop(&mut self) {
        self.manager.end_change_observation();
    }
}

impl RowLockManager {
    pub(crate) fn begin_change_observation(self: &Arc<Self>) -> RowChangeObservation {
        self.state.lock().active_change_observers += 1;
        RowChangeObservation {
            manager: Arc::clone(self),
        }
    }

    fn end_change_observation(&self) {
        let mut state = self.state.lock();
        state.active_change_observers = state.active_change_observers.saturating_sub(1);
        remove_inactive_versions(&mut state);
    }

    pub(crate) fn current_change_epoch(&self) -> u64 {
        self.state.lock().change_epoch
    }

    #[cfg(test)]
    pub(crate) fn current_row_version(&self, table: &str, doc_id: DocId) -> u64 {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        self.state
            .lock()
            .changes
            .iter()
            .rev()
            .find(|change| change.key == key)
            .map_or(0, |change| change.epoch)
    }

    pub(crate) fn conflicting_change_target_after(
        &self,
        table: &str,
        doc_id: DocId,
        baseline: RowChangeBaseline,
        wanted: LockStrength,
    ) -> Result<RowChangeTarget, SQLError> {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            return coordinator
                .change_target_after(
                    table_hash(table.as_bytes()),
                    doc_id,
                    baseline.cross_sequence,
                    wanted,
                )
                .map_err(SQLError::Internal);
        }
        Ok(resolve_local_change_target(
            &self.state.lock().changes,
            key,
            baseline.epoch,
            wanted,
        ))
    }

    /// Follow committed primary-key rewrites from `doc_id` to the final identity, considering only rewrites newer than the statement snapshot. This prevents an old update chain from attaching to a later row that reused the same primary key.
    pub(crate) fn row_successor_after(
        &self,
        table: &str,
        doc_id: DocId,
        baseline: RowChangeBaseline,
    ) -> Result<RowChangeTarget, SQLError> {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            return coordinator
                .change_target_after(
                    table_hash(table.as_bytes()),
                    doc_id,
                    baseline.cross_sequence,
                    LockStrength::ForUpdate,
                )
                .map_err(SQLError::Internal);
        }
        Ok(resolve_local_change_target(
            &self.state.lock().changes,
            key,
            baseline.epoch,
            LockStrength::ForUpdate,
        ))
    }

    pub(crate) fn publish_row_changes(
        &self,
        session_id: u64,
        changes: impl IntoIterator<Item = PendingRowChange>,
    ) -> Result<(), SQLError> {
        let changes = normalize_pending_row_changes(changes);
        if changes.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock();
        state.change_epoch = state.change_epoch.wrapping_add(1);
        let change_epoch = state.change_epoch;
        let committed = changes
            .into_iter()
            .filter_map(|change| {
                let kind = match change.kind {
                    PendingRowChangeKind::Insert => return None,
                    PendingRowChangeKind::Update => CommittedRowChangeKind::Update,
                    PendingRowChangeKind::Delete => CommittedRowChangeKind::Delete,
                    PendingRowChangeKind::Rewrite(successor) => {
                        CommittedRowChangeKind::Rewrite(successor)
                    }
                };
                let strength = match kind {
                    CommittedRowChangeKind::Rewrite(_) | CommittedRowChangeKind::Delete => {
                        LockStrength::ForUpdate
                    }
                    CommittedRowChangeKind::Update => {
                        mutation_strength(&state, session_id, change.key)
                    }
                };
                Some(CommittedRowChange {
                    epoch: change_epoch,
                    key: change.key,
                    kind,
                    strength,
                })
            })
            .collect::<Vec<_>>();
        let publication_result = if let Some(CrossAttachment::Active(coordinator)) =
            self.cross.as_ref()
        {
            let published = committed
                .iter()
                .map(|change| cross_process::PublishedRowChange {
                    table_hash: table_hash(&self.relation_bytes(change.key.table)),
                    doc_id: change.key.doc_id,
                    kind: match change.kind {
                        CommittedRowChangeKind::Update => {
                            cross_process::PublishedRowChangeKind::Update
                        }
                        CommittedRowChangeKind::Delete => {
                            cross_process::PublishedRowChangeKind::Delete
                        }
                        CommittedRowChangeKind::Rewrite(successor) => {
                            cross_process::PublishedRowChangeKind::Rewrite(
                                cross_process::PublishedRowIdentity {
                                    table_hash: table_hash(&self.relation_bytes(successor.table)),
                                    doc_id: successor.doc_id,
                                },
                            )
                        }
                    },
                    strength: change.strength,
                })
                .collect::<Vec<_>>();
            coordinator
                .publish_changes(&published)
                .map_err(SQLError::Internal)
        } else {
            Ok(())
        };
        for change in committed {
            let key = change.key;
            if state.active_change_observers != 0
                || state.rows.contains_key(&key)
                || row_has_waiter(&state, key)
            {
                state.changes.push(change);
            }
        }
        publication_result
    }
}
