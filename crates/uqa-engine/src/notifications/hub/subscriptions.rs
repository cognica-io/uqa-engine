//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic independent-listener registration against the retained hub boundary.

use super::super::registration;
use super::super::{inbox::SubscriptionInbox, subscription::NotificationSubscriptionError};
use super::{Arc, NotificationHub, NotificationListener};
use uqa_core::CancellationToken;

impl NotificationHub {
    pub(in crate::notifications) fn retire_subscription(&self, session_id: u64) {
        let _gate = self.commit_gate.lock();
        let removed = {
            let mut state = self.state.lock();
            let removed = state.listeners.remove(&session_id);
            Self::remove_consumed_entries(&mut state);
            removed
        };
        // The native lease is the lifetime authority. Releasing it makes any
        // remaining registry row dead; existing registration/publication scans
        // reap that row before using its cursor. Retirement must not acquire
        // the shared registry writer or cancel another listener's recovery.
        drop(removed);
    }

    pub(in crate::notifications) fn register_subscription(
        &self,
        session_id: u64,
        process_id: i32,
        channels: Vec<String>,
        inbox: &Arc<SubscriptionInbox>,
        cancellation: &CancellationToken,
    ) -> Result<(), NotificationSubscriptionError> {
        let source_error = registration::failure;
        if let Some(cross_state) = self.cross.as_ref() {
            let cross = cross_state
                .coordinator_with_cancellation(Some(cancellation))
                .map_err(source_error)?;
            let control = cross
                .recovery_control_with_cancellation(Some(cancellation))
                .map_err(source_error)?;
            let registry = cross
                .begin_registry_transaction_with_cancellation(Some(cancellation))
                .map_err(source_error)?;
            let _gate =
                registration::lock(&self.commit_gate, Some(cancellation)).map_err(source_error)?;
            let mut state =
                registration::lock(&self.state, Some(cancellation)).map_err(source_error)?;
            Self::remove_dead_listeners(&mut state);
            let queue = Self::load_cross_queue_state(&registry).map_err(source_error)?;
            let prepared = Self::prepare_cross_subscription(
                &cross, &registry, &state, queue, session_id, process_id, &channels,
            )
            .map_err(source_error)?;
            Self::cleanup_cross_entries(&registry, &prepared.listeners, queue.next_sequence)
                .map_err(source_error)?;
            registry
                .commit_with_control(&control)
                .map_err(source_error)?;
            state.listeners.insert(
                session_id,
                NotificationListener {
                    process_id,
                    channels,
                    queue: std::sync::Weak::new(),
                    wake: std::sync::Weak::new(),
                    subscription: Some(Arc::downgrade(inbox)),
                    next_sequence: queue.next_sequence,
                    position: queue.head_position,
                    transaction_open: false,
                    lease: prepared.new_lease,
                },
            );
            return Ok(());
        }
        let _gate =
            registration::lock(&self.commit_gate, Some(cancellation)).map_err(source_error)?;
        let mut state =
            registration::lock(&self.state, Some(cancellation)).map_err(source_error)?;
        Self::remove_dead_listeners(&mut state);
        let next_sequence = state.next_sequence;
        let position = state.head_position;
        state.listeners.insert(
            session_id,
            NotificationListener {
                process_id,
                channels,
                queue: std::sync::Weak::new(),
                wake: std::sync::Weak::new(),
                subscription: Some(Arc::downgrade(inbox)),
                next_sequence,
                position,
                transaction_open: false,
                lease: None,
            },
        );
        Ok(())
    }
}
