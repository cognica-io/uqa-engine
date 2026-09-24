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
    ) -> Result<(), SQLError> {
        if committed.storage_savepoint.is_none() {
            self.session.state.write().graph_overlay = None;
            self.restore_local_runtime_parameters();
            let publication_result = self.row_locks.publish_row_changes(
                self.session_id,
                committed.row_changes.iter().map(|change| change.pending),
            );
            drop(change_publication);
            self.row_locks.release_session(self.session_id);
            self.publish_committed_transaction_epochs();
            if !committed.statistics_changes.is_empty() {
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
        if let Some(parent) = stack.last_mut() {
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
