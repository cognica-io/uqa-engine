//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained notification resources follow the authoritative storage transaction outcome.

use super::{
    CrossNotificationRequest, CrossProcessState, Engine, NotificationCommitGuard,
    NotificationSessionCommit, SQLError,
};

impl Engine {
    pub(crate) fn prepares_persistent_notification(&self, frame: &crate::TransactionFrame) -> bool {
        self.notification_hub.cross.is_some()
            && (!frame.pending_notifications.is_empty() || !frame.pending_listen_actions.is_empty())
    }

    pub(super) fn prepare_notification_recovery(&self) -> Result<(), SQLError> {
        if let (Some(cross), Some(backend)) = (
            self.notification_hub.cross.as_ref(),
            self.storage.backend.as_ref(),
        ) {
            cross
                .coordinator()?
                .initialize_recovery(backend, &self.query_retention_control()?)?;
        }
        Ok(())
    }

    pub(crate) fn begin_notification_commit<'a>(
        &'a self,
        outer: bool,
        transaction: &crate::TransactionFrame,
    ) -> Result<Option<NotificationCommitGuard<'a>>, SQLError> {
        if !outer {
            return Ok(None);
        }
        if let Some(prepared) = transaction.pending_notification_commit.lock().take() {
            return Ok(Some(NotificationCommitGuard {
                _gate: self.notification_hub.commit_gate.lock(),
                cross: Some(prepared),
            }));
        }
        let current_channels = self.session.state.read().listened_channels.clone();
        if current_channels.is_empty()
            && transaction.pending_listen_actions.is_empty()
            && transaction.pending_notifications.is_empty()
        {
            return Ok(None);
        }
        self.prepare_notification_recovery()?;
        let control = self.query_retention_control()?;
        let final_channels = transaction.final_listened_channels(&current_channels);
        let cross = self
            .notification_hub
            .cross
            .as_ref()
            .map(CrossProcessState::coordinator)
            .transpose()?;
        let registry = cross
            .as_ref()
            .map(|cross| cross.begin_registry_transaction())
            .transpose()?;
        let commit = self.notification_hub.commit_gate.lock();
        let prepared = if let (Some(cross), Some(registry)) = (cross, registry) {
            let prepared = self.notification_hub.prepare_cross_commit(
                &cross,
                registry,
                CrossNotificationRequest {
                    session_id: self.session_id,
                    process_id: self.backend_process_id(),
                    channels: &final_channels,
                    pending: &transaction.pending_notifications,
                    control: &control,
                    durable_publication: self.prepares_persistent_notification(transaction),
                },
            )?;
            if let Some(publication) = prepared.publication.as_ref() {
                let store = self
                    .storage
                    .backend
                    .as_ref()
                    .and_then(|backend| backend.notification_publications())
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "persistent backend omitted atomic notification publication".into(),
                        )
                    })?;
                store
                    .stage_notification_publication_after(
                        publication,
                        prepared.previous_publication,
                    )
                    .map_err(|error| {
                        uqa_execution::storage_errors::storage_error(
                            "prepare atomic notification publication",
                            &error,
                        )
                    })?;
            }
            Some(Box::new(prepared))
        } else {
            self.notification_hub.validate_commit(
                &commit,
                self.session_id,
                &final_channels,
                &transaction.pending_notifications,
            )?;
            None
        };
        Ok(Some(NotificationCommitGuard {
            _gate: commit,
            cross: prepared,
        }))
    }

    pub(crate) fn commit_notification_state(
        &self,
        commit: NotificationCommitGuard<'_>,
        transaction: &crate::TransactionFrame,
    ) -> Result<(), SQLError> {
        let current_channels = self.session.state.read().listened_channels.clone();
        let channels = transaction.final_listened_channels(&current_channels);
        let session = NotificationSessionCommit {
            session_id: self.session_id,
            process_id: self.backend_process_id(),
            channels: channels.clone(),
            queue: &self.runtime.notifications,
            wake: &self.runtime.notification_wake,
            notices: &self.runtime.notices,
            pending: &transaction.pending_notifications,
        };
        let NotificationCommitGuard { _gate: gate, cross } = commit;
        self.session.state.write().listened_channels = channels;
        if let Some(prepared) = cross {
            self.notification_hub
                .finalize_cross_commit(gate, *prepared, session, true)
        } else {
            self.notification_hub.commit_session(&gate, session);
            Ok(())
        }
    }
}
