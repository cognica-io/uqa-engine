//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// The receive is admitted before close starts; no new use of its retired handle is allowed.
#![allow(unsafe_code)]

use std::{ptr, time::Duration};
use uqa_ffi::{
    uqa_engine_close, uqa_engine_execute, uqa_engine_open, uqa_engine_open_memory,
    uqa_engine_open_session, uqa_notification_create, uqa_notification_destroy,
    uqa_notification_get_payload, uqa_notification_get_sequence, uqa_result_create,
    uqa_result_destroy, uqa_subscription_close, uqa_subscription_open, uqa_subscription_poll,
    uqa_subscription_wait, UQANotification, UQAStringView, UQASubscription, UQASubscriptionOptions,
    UQA_DONE, UQA_ERROR_BUSY, UQA_ERROR_STATE, UQA_OK, UQA_SUBSCRIPTION_OPTIONS_VERSION,
    UQA_TIMEOUT,
};

fn options() -> UQASubscriptionOptions {
    UQASubscriptionOptions {
        struct_size: size_of::<UQASubscriptionOptions>() as u32,
        version: UQA_SUBSCRIPTION_OPTIONS_VERSION,
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 1,
        max_queued_bytes: 8192,
        max_registry_entries_per_poll: 16,
    }
}

#[test]
fn close_wakes_an_admitted_receiver_and_retires_its_owner() {
    unsafe {
        let mut engine = ptr::null_mut();
        let mut subscription = ptr::null_mut();
        let mut waiting = ptr::null_mut();
        let mut probe = ptr::null_mut();
        let options = options();
        assert_eq!(
            uqa_engine_open_memory(ptr::null_mut(), &raw mut engine),
            UQA_OK
        );
        assert_eq!(uqa_notification_create(&raw mut waiting), UQA_OK);
        assert_eq!(uqa_notification_create(&raw mut probe), UQA_OK);
        assert_eq!(
            uqa_subscription_open(
                engine,
                [c"jobs".as_ptr()].as_ptr(),
                1,
                &raw const options,
                &raw mut subscription
            ),
            UQA_OK
        );
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        let address = subscription as usize;
        let destination = waiting as usize;
        let (entered, receive_entered) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            // The probe may have won the admission race. Retry only BUSY until admitted.
            loop {
                let outcome = uqa_subscription_wait(
                    address as *mut UQASubscription,
                    u64::MAX,
                    destination as *mut UQANotification,
                );
                if outcome != UQA_ERROR_BUSY {
                    return outcome;
                }
                std::thread::yield_now();
            }
        });
        receive_entered
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let started = std::time::Instant::now();
        loop {
            let outcome = uqa_subscription_poll(subscription, probe);
            if outcome == UQA_ERROR_BUSY {
                break;
            }
            assert_eq!(outcome, UQA_TIMEOUT);
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::yield_now();
        }
        assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
        assert!(subscription.is_null());
        assert_eq!(worker.join().unwrap(), UQA_DONE);
        let mut sequence = 99;
        assert_eq!(
            uqa_notification_get_sequence(waiting, &raw mut sequence),
            UQA_ERROR_STATE
        );
        assert_eq!(sequence, 0);
        assert_eq!(uqa_notification_destroy(&raw mut waiting), UQA_OK);
        assert_eq!(uqa_notification_destroy(&raw mut probe), UQA_OK);
    }
}

#[test]
fn retained_provider_delivers_after_the_creating_engine_closes() {
    unsafe {
        let directory = tempfile::tempdir().unwrap();
        let path =
            std::ffi::CString::new(directory.path().join("events.db").to_str().unwrap()).unwrap();
        let mut engine = ptr::null_mut();
        let mut publisher = ptr::null_mut();
        let mut subscription = ptr::null_mut();
        let mut notification = ptr::null_mut();
        let mut result = ptr::null_mut();
        let options = options();
        assert_eq!(
            uqa_engine_open(path.as_ptr(), ptr::null_mut(), &raw mut engine),
            UQA_OK
        );
        assert_eq!(uqa_engine_open_session(engine, &raw mut publisher), UQA_OK);
        assert_eq!(uqa_notification_create(&raw mut notification), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_subscription_open(
                engine,
                [c"jobs".as_ptr()].as_ptr(),
                1,
                &raw const options,
                &raw mut subscription
            ),
            UQA_OK
        );
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                publisher,
                c"NOTIFY jobs, 'after creator close'".as_ptr(),
                result
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut publisher), UQA_OK);
        assert_eq!(
            uqa_subscription_wait(subscription, 1000, notification),
            UQA_OK
        );
        assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
        let mut payload = UQAStringView::default();
        assert_eq!(
            uqa_notification_get_payload(notification, &raw mut payload),
            UQA_OK
        );
        assert_eq!(
            std::slice::from_raw_parts(payload.data.cast::<u8>(), payload.size),
            b"after creator close"
        );
        assert_eq!(uqa_notification_destroy(&raw mut notification), UQA_OK);
        std::fs::rename(
            directory.path().join("events.db"),
            directory.path().join("closed.db"),
        )
        .unwrap();
    }
}
