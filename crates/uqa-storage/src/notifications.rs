//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Notification payloads and bounded queue page accounting.

pub mod conformance;
mod publication;
mod registry;
pub use publication::{
    NotificationMessageRef, NotificationPublication, NotificationPublicationHeader,
    NotificationPublicationStart, NotificationPublicationView, NotificationSubscriptionView,
};
pub use registry::{NotificationListenerRow, NotificationQueueEntry, NotificationQueueState};

/// Fresh committed publication access is independent of the caller's SQL snapshot. Publication is an auxiliary effect of its current transaction, not a user-data write.
pub trait NotificationPublicationStore: Send + Sync {
    fn stage_notification_publication(
        &self,
        publication: &NotificationPublication,
    ) -> crate::StorageBackendResult<()>;

    fn visit_notification_publication(
        &self,
        control: &crate::read_control::StorageReadControl,
        visit: &mut dyn FnMut(
            Option<NotificationPublicationView<'_>>,
        ) -> crate::StorageBackendResult<()>,
    ) -> crate::StorageBackendResult<()>;

    /// Clear only the acknowledged fingerprint through autonomous committed state. The caller must serialize queue acknowledgement and new publication and supply uncancelled completion control.
    fn acknowledge_notification_publication(
        &self,
        fingerprint: [u8; 32],
        control: &crate::read_control::StorageReadControl,
    ) -> crate::StorageBackendResult<()>;
}

pub const MAX_NOTIFICATION_CHANNEL_BYTES: usize = 64;
pub const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 8_000;
pub const NOTIFICATION_QUEUE_PAGE_BYTES: u64 = 8_192;
pub const MAX_NOTIFICATION_QUEUE_PAGES: u64 = 1_048_576;
const NOTIFICATION_ENTRY_HEADER_BYTES: u64 = 16;
const MIN_NOTIFICATION_ENTRY_BYTES: u64 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingNotification {
    pub channel: String,
    pub payload: String,
}

pub fn notification_end_position(position: u64, notification: &PendingNotification) -> u64 {
    end_position(
        position,
        notification.channel.len(),
        notification.payload.len(),
    )
}

fn end_position(position: u64, channel_bytes: usize, payload_bytes: usize) -> u64 {
    let content = NOTIFICATION_ENTRY_HEADER_BYTES
        .saturating_add(channel_bytes as u64)
        .saturating_add(1)
        .saturating_add(payload_bytes as u64)
        .saturating_add(1);
    let length = content.saturating_add(3) & !3;
    let offset = position % NOTIFICATION_QUEUE_PAGE_BYTES;
    let aligned_position = if offset.saturating_add(length) > NOTIFICATION_QUEUE_PAGE_BYTES {
        position.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - offset)
    } else {
        position
    };
    let end = aligned_position.saturating_add(length);
    let end_offset = end % NOTIFICATION_QUEUE_PAGE_BYTES;
    if end_offset.saturating_add(MIN_NOTIFICATION_ENTRY_BYTES) > NOTIFICATION_QUEUE_PAGE_BYTES {
        end.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - end_offset)
    } else {
        end
    }
}

pub fn notifications_fit_queue(
    mut head: u64,
    tail: u64,
    max_queue_pages: u64,
    pending: &[PendingNotification],
) -> bool {
    let tail_page = queue_page(tail);
    for notification in pending {
        if queue_page(head).saturating_sub(tail_page) >= max_queue_pages {
            return false;
        }
        let content = NOTIFICATION_ENTRY_HEADER_BYTES
            .saturating_add(notification.channel.len() as u64)
            .saturating_add(1)
            .saturating_add(notification.payload.len() as u64)
            .saturating_add(1);
        let length = content.saturating_add(3) & !3;
        let offset = head % NOTIFICATION_QUEUE_PAGE_BYTES;
        if offset.saturating_add(length) > NOTIFICATION_QUEUE_PAGE_BYTES {
            head = head.saturating_add(NOTIFICATION_QUEUE_PAGE_BYTES - offset);
            if queue_page(head).saturating_sub(tail_page) >= max_queue_pages {
                return false;
            }
        }
        head = notification_end_position(head, notification);
    }
    true
}

pub fn queue_page(position: u64) -> u64 {
    position / NOTIFICATION_QUEUE_PAGE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(channel_bytes: usize, payload_bytes: usize) -> PendingNotification {
        PendingNotification {
            channel: "c".repeat(channel_bytes),
            payload: "p".repeat(payload_bytes),
        }
    }

    #[test]
    fn queue_layout_accounts_for_alignment_page_padding_and_capacity() {
        let largest = pending(63, 7_999);
        let smallest = pending(1, 0);
        assert_eq!(notification_end_position(0, &largest), 8_080);
        assert_eq!(notification_end_position(8_080, &smallest), 8_100);

        let mut one_page = vec![largest];
        one_page.extend(std::iter::repeat_n(smallest.clone(), 5));
        assert!(notifications_fit_queue(0, 0, 1, &one_page));
        assert!(!notifications_fit_queue(
            0,
            0,
            1,
            &[one_page, vec![smallest]].concat()
        ));

        let entry_that_requires_the_next_page = pending(1, 30);
        assert!(!notifications_fit_queue(
            8_160,
            0,
            1,
            &[entry_that_requires_the_next_page]
        ));
    }
}
