//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication of durable transaction changes after the backend commit.

use std::sync::atomic::Ordering;

use super::{Engine, SQLError, TransactionFrame};
use crate::notifications::NotificationCommitGuard;
use uqa_execution::row_locks::RowChangePublication;

impl Engine {
    pub(super) fn publish_committed_transaction_frame(
        &self,
        stack: &mut [TransactionFrame],
        committed: TransactionFrame,
        change_publication: Option<RowChangePublication<'_>>,
        notification_commit: Option<NotificationCommitGuard<'_>>,
        wrote_records: bool,
    ) -> Result<(), SQLError> {
        if committed.storage_savepoint.is_none() {
            committed.prepared_changes.invalidate_with_routines(
                self.session.prepared.write().values_mut(),
                &self.session.routine_bodies,
            );
            self.session.state.write().graph_overlay = None;
            self.restore_local_runtime_parameters();
            let publication_result = self.row_locks.publish_row_changes(
                self.session_id,
                committed.row_changes.iter().map(|change| change.pending),
            );
            drop(change_publication);
            self.row_locks.release_session(self.session_id);
            let data_only = wrote_records
                && self.epochs.table_data.dirty.load(Ordering::Acquire)
                && !self.epochs.table_catalog.dirty.load(Ordering::Acquire)
                && !self.epochs.catalog_registry.dirty.load(Ordering::Acquire);
            self.publish_committed_transaction_epochs();
            if data_only {
                self.adopt_own_commit_revisions();
            }
            let recorded = committed.statistics_settlement.recorded();
            self.settle_statistics_changes(committed.statistics_settlement.clone());
            // A commit that wrote no maintenance record changed nothing the worker decides by.
            if recorded {
                self.wake_automatic_statistics();
            }
            let notification_result = notification_commit.map_or(Ok(()), |notification_commit| {
                self.commit_notification_state(notification_commit, &committed)
            });
            self.session
                .portals
                .lock()
                .retain(|_, portal| portal.holdable);
            publication_result?;
            notification_result?;
        }
        let nested_savepoint = committed.storage_savepoint;
        if let Some(parent) = stack.last_mut() {
            parent.prepared_changes.append(committed.prepared_changes);
            parent.next_lock_mark = parent.next_lock_mark.max(committed.next_lock_mark);
            parent.constraint_modes = committed.constraint_modes;
            parent.row_changes.extend(committed.row_changes);
            Self::merge_statistics_changes(
                &mut parent.statistics_changes,
                committed.statistics_changes,
            );
            parent.deferred_foreign_key_checks = committed.deferred_foreign_key_checks;
            parent.deferred_constraint_trigger_events =
                committed.deferred_constraint_trigger_events;
            parent.merge_pending_listen_actions(committed.pending_listen_actions);
            parent.merge_pending_notifications(committed.pending_notifications);
            parent.first_snapshot_set |= committed.first_snapshot_set;
        }
        if let Some(savepoint) = nested_savepoint {
            // The parent keeps the nested frame's changes.
            super::fixed_identities::follow_identities(stack, |identities| {
                identities.release(savepoint)
            });
        }
        Ok(())
    }

    pub(super) fn publish_committed_transaction_epochs(&self) {
        if self.epochs.table_catalog.dirty.load(Ordering::Acquire) {
            self.publish_table_catalog_changes();
        }
        if self.epochs.catalog_registry.dirty.load(Ordering::Acquire) {
            self.publish_catalog_registry_changes();
        }
        if self.epochs.table_data.dirty.load(Ordering::Acquire) {
            self.publish_table_data_changes();
        }
    }
}
