//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe publication independently of the invoking session's failed or poisoned state.

use super::*;

pub(super) struct Events {
    subscription: *mut UQASubscription,
    notification: *mut UQANotification,
    original_sequence: u64,
}

impl Events {
    pub(super) unsafe fn new(db: &Database) -> Self {
        let options = UQASubscriptionOptions {
            struct_size: size_of::<UQASubscriptionOptions>() as u32,
            version: UQA_SUBSCRIPTION_OPTIONS_VERSION,
            max_active_subscriptions: 1,
            max_channels: 1,
            max_queued_notifications: 4,
            max_queued_bytes: 8192,
            max_registry_entries_per_poll: 16,
        };
        let channels = [c"combined_events".as_ptr()];
        let mut result = Self {
            subscription: ptr::null_mut(),
            notification: ptr::null_mut(),
            original_sequence: 0,
        };
        assert_eq!(
            uqa_subscription_open(
                db.engine,
                channels.as_ptr(),
                1,
                &raw const options,
                &raw mut result.subscription
            ),
            UQA_OK
        );
        assert_eq!(
            uqa_notification_create(&raw mut result.notification),
            UQA_OK
        );
        db.sql(c"NOTIFY combined_events, 'original'");
        assert_eq!(
            uqa_subscription_wait(result.subscription, 5000, result.notification),
            UQA_OK
        );
        result.payload(b"original");
        assert_eq!(
            uqa_notification_get_sequence(result.notification, &raw mut result.original_sequence),
            UQA_OK
        );
        result
    }

    unsafe fn payload(&self, expected: &[u8]) {
        let mut payload = UQAStringView::default();
        assert_eq!(
            uqa_notification_get_payload(self.notification, &raw mut payload),
            UQA_OK
        );
        assert_eq!(
            std::slice::from_raw_parts(payload.data.cast::<u8>(), payload.size),
            expected
        );
    }

    pub(super) unsafe fn absent(&self) {
        assert_eq!(
            uqa_subscription_poll(self.subscription, self.notification),
            UQA_TIMEOUT
        );
        self.payload(b"original");
    }

    pub(super) unsafe fn receive_after_rejected_transfer(&self, db: &Database) {
        let bytes = db.bytes();
        assert_eq!(
            target::reject_named("uqa::notification::NotificationData", 0, || {
                uqa_subscription_wait(self.subscription, 5000, self.notification)
            }),
            UQA_ERROR_OUT_OF_MEMORY
        );
        assert_eq!(db.bytes(), bytes);
        self.payload(b"original");
        assert_eq!(
            uqa_subscription_wait(self.subscription, 5000, self.notification),
            UQA_OK
        );
        self.payload(b"committed");
        let mut sequence = 0;
        assert_eq!(
            uqa_notification_get_sequence(self.notification, &raw mut sequence),
            UQA_OK
        );
        assert_eq!(sequence, self.original_sequence + 1);
        assert_eq!(
            uqa_subscription_poll(self.subscription, self.notification),
            UQA_TIMEOUT
        );
        self.payload(b"committed");
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        unsafe {
            let subscription = uqa_subscription_close(&raw mut self.subscription);
            let notification = uqa_notification_destroy(&raw mut self.notification);
            if !std::thread::panicking() {
                assert_eq!((subscription, notification), (UQA_OK, UQA_OK));
            }
        }
    }
}
