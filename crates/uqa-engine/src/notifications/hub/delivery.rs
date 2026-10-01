//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained listener delivery at the committed hub boundary.

use super::super::{inbox::SubscriptionInbox, subscription::NotificationSubscriptionError};
use super::{
    Arc, CrossProcessListenerRow, CrossProcessQueueState, CrossProcessRegistryTransaction,
    NotificationHub, NotificationHubState, PreparedDelivery, SQLError, SQLNotification,
};
use std::{num::NonZeroUsize, ops::ControlFlow};
use uqa_core::notifications::NotificationFailureKind;
use uqa_storage::{
    notifications::{notification_parts_end_position, NotificationListenerKey},
    read_control::StorageReadControl,
};

impl NotificationHub {
    pub(super) fn complete_deliveries(
        result: Result<(), SQLError>,
        state: &mut NotificationHubState,
        deliveries: Vec<PreparedDelivery>,
    ) -> Result<(), SQLError> {
        if let Err(error) = result {
            state.delivery_failures = state.delivery_failures.wrapping_add(1);
            state.last_delivery_failure = Some(error.clone());
            let failure = NotificationSubscriptionError::with_source(
                NotificationFailureKind::SourceUnavailable,
                error.clone(),
            );
            for listener in state.listeners.values() {
                if let Some(inbox) = listener
                    .subscription
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                {
                    inbox.fail(failure.clone());
                }
            }
            return Err(error);
        }
        Self::apply_deliveries(state, deliveries);
        Self::remove_dead_listeners(state);
        Ok(())
    }

    pub(super) fn cross_deliveries(
        registry: &CrossProcessRegistryTransaction,
        queue_state: CrossProcessQueueState,
        state: &NotificationHubState,
        control: &StorageReadControl,
    ) -> Result<Vec<PreparedDelivery>, SQLError> {
        let mut deliveries = Vec::new();
        for (session_id, local) in &state.listeners {
            let Some(lease) = local.lease.as_ref() else {
                continue;
            };
            let owner_id = lease.owner_id();
            let inbox = local
                .subscription
                .as_ref()
                .and_then(std::sync::Weak::upgrade);
            if local.subscription.is_some() && inbox.is_none() {
                registry.drop_listener(owner_id, *session_id)?;
                continue;
            }
            let Some(mut listener) = registry.listener(
                NotificationListenerKey {
                    owner_id,
                    session_id: *session_id,
                },
                inbox.as_ref().map(|inbox| inbox.options.max_channels),
            )?
            else {
                if let Some(inbox) = inbox {
                    inbox.fail(NotificationSubscriptionError::new(
                        NotificationFailureKind::SourceUnavailable,
                    ));
                }
                continue;
            };
            if listener.transaction_open {
                continue;
            }
            if let Some(inbox) = inbox {
                let batch = Self::prepare_bounded_delivery(
                    registry,
                    &mut listener,
                    queue_state,
                    &inbox,
                    control,
                )?;
                deliveries.push(PreparedDelivery::Subscription(batch));
                continue;
            }
            let entries = registry.entries_from(listener.next_sequence)?;
            let notifications = entries
                .into_iter()
                .filter(|entry| listener.channels.contains(&entry.channel))
                .map(|entry| SQLNotification {
                    process_id: entry.process_id,
                    channel: entry.channel,
                    payload: entry.payload,
                })
                .collect::<Vec<_>>();
            if listener.next_sequence != queue_state.next_sequence
                || listener.position != queue_state.head_position
            {
                listener.next_sequence = queue_state.next_sequence;
                listener.position = queue_state.head_position;
                Self::save_cross_listener(registry, &listener)?;
            }
            if !notifications.is_empty() {
                deliveries.push(PreparedDelivery::Session {
                    session_id: *session_id,
                    notifications,
                });
            }
        }
        Ok(deliveries)
    }

    pub(super) fn apply_deliveries(
        state: &NotificationHubState,
        deliveries: Vec<PreparedDelivery>,
    ) {
        for delivery in deliveries {
            let (session_id, notifications) = match delivery {
                PreparedDelivery::Subscription(batch) => {
                    batch.commit();
                    continue;
                }
                PreparedDelivery::Session {
                    session_id,
                    notifications,
                } => (session_id, notifications),
            };
            let Some(listener) = state.listeners.get(&session_id) else {
                continue;
            };
            let Some(queue) = listener.queue.upgrade() else {
                continue;
            };
            queue.lock().extend(notifications);
            if let Some(wake) = listener.wake.upgrade() {
                wake.notify_all();
            }
        }
    }

    fn prepare_bounded_delivery(
        registry: &CrossProcessRegistryTransaction,
        listener: &mut CrossProcessListenerRow,
        queue_state: CrossProcessQueueState,
        inbox: &Arc<SubscriptionInbox>,
        control: &StorageReadControl,
    ) -> Result<super::super::inbox::PreparedNotifications, SQLError> {
        let mut batch = inbox.prepare();
        let mut next_sequence = listener.next_sequence;
        let mut position = listener.position;
        let progress = registry.visit_entries_from(
            listener.next_sequence,
            NonZeroUsize::new(inbox.options.max_registry_entries_per_poll)
                .expect("validated scan limit"),
            control,
            &mut |entry| {
                if entry.sequence != next_sequence {
                    inbox.fail(NotificationSubscriptionError::new(
                        NotificationFailureKind::SourceUnavailable,
                    ));
                    return Ok(ControlFlow::Break(()));
                }
                if listener
                    .channels
                    .iter()
                    .any(|channel| channel == entry.channel)
                    && !batch.push(entry.process_id, entry.channel, entry.payload)
                {
                    return Ok(ControlFlow::Break(()));
                }
                position = notification_parts_end_position(position, entry.channel, entry.payload);
                next_sequence = entry.sequence + 1;
                Ok(ControlFlow::Continue(()))
            },
        )?;
        if progress.exhausted && next_sequence != queue_state.next_sequence {
            inbox.fail(NotificationSubscriptionError::new(
                NotificationFailureKind::SourceUnavailable,
            ));
        }
        if inbox.is_closed() {
            registry.drop_listener(listener.owner_id, listener.session_id)?;
            listener.next_sequence = queue_state.next_sequence;
            listener.position = queue_state.head_position;
        } else if listener.next_sequence != progress.next_sequence {
            listener.next_sequence = progress.next_sequence;
            listener.position = position;
            Self::save_cross_listener(registry, listener)?;
        }
        Ok(batch)
    }

    pub(super) fn deliver_idle_listeners(state: &mut NotificationHubState) {
        let head_position = state.head_position;
        let next_sequence = state.next_sequence;
        let entries = &state.entries;
        let listeners = &mut state.listeners;
        let mut dead = Vec::new();
        for (session_id, listener) in listeners.iter_mut() {
            if listener.transaction_open {
                continue;
            }
            if let Some(inbox) = listener.subscription.as_ref() {
                if let Some(inbox) = inbox.upgrade().filter(|inbox| !inbox.is_closed()) {
                    for entry in entries
                        .iter()
                        .filter(|entry| entry.sequence >= listener.next_sequence)
                        .filter(|entry| listener.channels.contains(&entry.channel))
                    {
                        inbox.deliver(entry.process_id, &entry.channel, &entry.payload);
                        if inbox.is_closed() {
                            break;
                        }
                    }
                    listener.next_sequence = next_sequence;
                    listener.position = head_position;
                    if inbox.is_closed() {
                        dead.push(*session_id);
                    }
                } else {
                    dead.push(*session_id);
                }
                continue;
            }
            let Some(queue) = listener.queue.upgrade() else {
                dead.push(*session_id);
                continue;
            };
            let delivered = entries
                .iter()
                .filter(|entry| entry.sequence >= listener.next_sequence)
                .filter(|entry| listener.channels.contains(&entry.channel))
                .map(|entry| SQLNotification {
                    process_id: entry.process_id,
                    channel: entry.channel.clone(),
                    payload: entry.payload.clone(),
                })
                .collect::<Vec<_>>();
            queue.lock().extend(delivered);
            if let Some(wake) = listener.wake.upgrade() {
                wake.notify_all();
            }
            listener.next_sequence = next_sequence;
            listener.position = head_position;
        }
        for session_id in dead {
            listeners.remove(&session_id);
        }
    }
}
