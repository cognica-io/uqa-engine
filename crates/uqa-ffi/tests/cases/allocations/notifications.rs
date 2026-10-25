//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed ownership transfer preserves queued notices, notifications and subscription capacity.

use super::{
    fixtures::{Database, PROVIDERS},
    target,
};
use std::ptr;
use uqa_ffi::*;

fn options() -> UQASubscriptionOptions {
    UQASubscriptionOptions {
        struct_size: size_of::<UQASubscriptionOptions>() as u32,
        version: UQA_SUBSCRIPTION_OPTIONS_VERSION,
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 4,
        max_queued_bytes: 8192,
        max_registry_entries_per_poll: 16,
    }
}

unsafe fn payload(notification: *mut UQANotification, expected: &[u8]) {
    let mut text = UQAStringView::default();
    assert_eq!(
        uqa_notification_get_payload(notification, &raw mut text),
        UQA_OK
    );
    assert_eq!(
        std::slice::from_raw_parts(text.data.cast::<u8>(), text.size),
        expected
    );
}

#[test]
fn failed_subscription_and_notification_allocation_preserves_capacity_and_queue() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        let options = options();
        let channels = [c"allocation_channel".as_ptr()];
        let mut subscription = ptr::null_mut();
        let mut notification = ptr::null_mut();
        unsafe {
            let bytes = db.bytes();
            assert_eq!(
                target::reject::<UQASubscription, _>(|| uqa_subscription_open(
                    db.engine,
                    channels.as_ptr(),
                    1,
                    &raw const options,
                    &raw mut subscription
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(subscription.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(
                uqa_subscription_open(
                    db.engine,
                    channels.as_ptr(),
                    1,
                    &raw const options,
                    &raw mut subscription
                ),
                UQA_OK
            );
            assert_eq!(uqa_notification_create(&raw mut notification), UQA_OK);
            db.sql(c"NOTIFY allocation_channel, 'original'");
            assert_eq!(
                uqa_subscription_wait(subscription, 5000, notification),
                UQA_OK
            );
            payload(notification, b"original");
            db.sql(c"NOTIFY allocation_channel, 'replacement'");
            let bytes = db.bytes();
            assert_eq!(
                target::reject_named("uqa_ffi::notification::NotificationData", 0, || {
                    uqa_subscription_wait(subscription, 5000, notification)
                }),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert_eq!(db.bytes(), bytes);
            payload(notification, b"original");
            assert_eq!(
                uqa_subscription_wait(subscription, 5000, notification),
                UQA_OK
            );
            payload(notification, b"replacement");
            assert_eq!(
                uqa_subscription_poll(subscription, notification),
                UQA_TIMEOUT
            );
            assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
            assert_eq!(
                uqa_subscription_open(
                    db.engine,
                    channels.as_ptr(),
                    1,
                    &raw const options,
                    &raw mut subscription
                ),
                UQA_OK
            );
            assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
            assert_eq!(uqa_notification_destroy(&raw mut notification), UQA_OK);
        }
    }
}

#[test]
fn failed_notice_allocation_preserves_the_native_queue_and_releases_admission() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        db.sql(c"DO $$ BEGIN RAISE NOTICE 'retained notice'; END $$");
        let bytes = db.bytes();
        let mut notice = ptr::null_mut();
        unsafe {
            assert_eq!(
                target::reject::<UQANotice, _>(|| uqa_notice_create(db.engine, &raw mut notice)),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(notice.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(uqa_notice_create(db.engine, &raw mut notice), UQA_OK);
            let mut text = UQAStringView::default();
            assert_eq!(uqa_notice_get_message(notice, &raw mut text), UQA_OK);
            assert_eq!(
                std::slice::from_raw_parts(text.data.cast::<u8>(), text.size),
                b"retained notice"
            );
            assert_eq!(uqa_notice_destroy(&raw mut notice), UQA_OK);
            assert_eq!(uqa_notice_create(db.engine, &raw mut notice), UQA_DONE);
            assert_eq!(db.bytes(), bytes);
        }
    }
}
