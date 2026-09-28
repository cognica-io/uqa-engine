//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Queue admission, unpublished delivery, receive cancellation and closed diagnostics.

use super::*;
use std::{
    future::Future,
    pin::pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Weak,
    },
    task::{Context, Wake, Waker},
};

struct ReceiverWake {
    inbox: Weak<SubscriptionInbox>,
    calls: AtomicUsize,
}

impl Wake for ReceiverWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        assert!(self.inbox.upgrade().unwrap().state.try_lock().is_some());
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for ReceiverWake {
    fn drop(&mut self) {
        if let Some(inbox) = self.inbox.upgrade() {
            assert!(inbox.state.try_lock().is_some());
        }
    }
}

fn receiver_wake(inbox: &Arc<SubscriptionInbox>) -> Arc<ReceiverWake> {
    Arc::new(ReceiverWake {
        inbox: Arc::downgrade(inbox),
        calls: AtomicUsize::new(0),
    })
}

#[test]
fn async_receive_waits_for_publication_and_retains_its_single_consumer_slot() {
    let inbox = inbox(options());
    let wake = receiver_wake(&inbox);
    let waker = Waker::from(wake.clone());
    let mut context = Context::from_waker(&waker);
    let mut receive = pin!(inbox.wait_async());
    assert!(receive.as_mut().poll(&mut context).is_pending());
    let mut batch = inbox.prepare();
    assert!(batch.push(7, "events", "committed"));
    assert_eq!(wake.calls.load(Ordering::Relaxed), 0);
    batch.commit();
    assert_eq!(wake.calls.load(Ordering::Relaxed), 1);
    // Waking the task has not yet released its ownership of the next value.
    let mut another = pin!(inbox.wait_async());
    let Poll::Ready(Err(error)) = another.as_mut().poll(&mut context) else {
        panic!("one async consumer")
    };
    assert_eq!(error.kind(), NotificationFailureKind::InvalidRequest);
    let Poll::Ready(Ok(Some(NotificationEvent::Notification {
        identity,
        sequence,
        notification,
    }))) = receive.as_mut().poll(&mut context)
    else {
        panic!("published event")
    };
    assert_eq!(identity, *inbox.identity());
    assert_eq!(sequence, 1);
    assert_eq!(notification.payload, "committed");
    assert_eq!(notification.process_id, 7);
}

#[test]
fn dropping_a_pending_async_receive_releases_its_waker_without_consuming_data() {
    let inbox = inbox(options());
    let wake = receiver_wake(&inbox);
    let weak_wake = Arc::downgrade(&wake);
    let waker = Waker::from(wake);
    let mut receive = Box::pin(inbox.wait_async());
    assert!(receive
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    drop(waker);
    drop(receive);
    assert!(weak_wake.upgrade().is_none());
    assert!(!inbox.state.lock().async_waiting);
    assert!(!inbox.is_closed());
    inbox.deliver(7, "events", "after cancellation");
    take(&inbox, 1, "after cancellation");
}

#[test]
fn async_receive_close_and_retained_failures_wake_without_polling_threads() {
    for kind in [
        None,
        Some(NotificationFailureKind::SourceUnavailable),
        Some(NotificationFailureKind::Backpressure),
    ] {
        let inbox = inbox(NotificationSubscriptionOptions {
            max_queued_notifications: 1,
            ..options()
        });
        let wake = receiver_wake(&inbox);
        let waker = Waker::from(wake.clone());
        let mut context = Context::from_waker(&waker);
        let mut receive = pin!(inbox.wait_async());
        assert!(receive.as_mut().poll(&mut context).is_pending());
        match kind {
            None => inbox.close(),
            Some(NotificationFailureKind::Backpressure) => {
                let mut batch = inbox.prepare();
                assert!(batch.push(7, "events", "private"));
                assert!(!batch.push(7, "events", "overflow"));
            }
            Some(kind) => inbox.fail(NotificationSubscriptionError::new(kind)),
        }
        assert_eq!(wake.calls.load(Ordering::Relaxed), 1);
        match receive.as_mut().poll(&mut context) {
            Poll::Ready(Ok(None)) => assert!(kind.is_none()),
            Poll::Ready(Err(error)) => assert_eq!(Some(error.kind()), kind),
            _ => panic!("terminal state"),
        }
        assert!(!inbox.state.lock().async_waiting);
    }
}

#[test]
fn repolling_a_pending_async_receive_replaces_its_executor_waker() {
    let inbox = inbox(options());
    let first = receiver_wake(&inbox);
    let second = receiver_wake(&inbox);
    let mut receive = pin!(inbox.wait_async());
    for wake in [&first, &second] {
        let waker = Waker::from(wake.clone());
        assert!(receive
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
    }
    inbox.deliver(7, "events", "ready");
    assert_eq!(first.calls.load(Ordering::Relaxed), 0);
    assert_eq!(second.calls.load(Ordering::Relaxed), 1);
}

fn options() -> NotificationSubscriptionOptions {
    NotificationSubscriptionOptions {
        max_active_subscriptions: 4,
        max_channels: 2,
        max_queued_notifications: 8,
        max_queued_bytes: 16_384,
        max_registry_entries_per_poll: 2,
    }
}

fn inbox(options: NotificationSubscriptionOptions) -> Arc<SubscriptionInbox> {
    SubscriptionInbox::new(
        NotificationIdentity {
            epoch: "919108f7-52d1-4320-9bac-f847db4148a8".parse().unwrap(),
            request_id: None,
        },
        options,
    )
}

fn take(inbox: &SubscriptionInbox, expected_sequence: u64, expected_payload: &str) {
    let Poll::Ready(Some(NotificationEvent::Notification {
        identity,
        sequence,
        notification,
    })) = inbox.poll().unwrap()
    else {
        panic!("one committed notification expected")
    };
    assert_eq!(&identity, inbox.identity());
    assert_eq!(sequence, expected_sequence);
    assert_eq!(
        notification,
        SQLNotification {
            process_id: 7,
            channel: "events".into(),
            payload: expected_payload.into()
        }
    );
}

#[test]
fn only_committed_preparations_become_visible_and_abort_releases_admission() {
    let inbox = inbox(options());
    let mut abandoned = inbox.prepare();
    assert!(abandoned.push(7, "events", "not committed"));
    assert!(matches!(inbox.poll().unwrap(), Poll::Pending));
    drop(abandoned);
    let mut committed = inbox.prepare();
    assert!(committed.push(7, "events", "first"));
    assert!(committed.push(7, "events", "second"));
    assert!(matches!(inbox.poll().unwrap(), Poll::Pending));
    committed.commit();
    take(&inbox, 1, "first");
    take(&inbox, 2, "second");
    assert_eq!(inbox.state.lock().reserved, 0);
    assert!(inbox.memory.peak() <= options().max_queued_bytes);
    inbox.close();
    assert_eq!(inbox.memory.used(), 0);
}

#[test]
fn count_admission_includes_private_batches_and_overflow_is_sticky() {
    let inbox = inbox(NotificationSubscriptionOptions {
        max_queued_notifications: 2,
        ..options()
    });
    let mut batch = inbox.prepare();
    assert!(batch.push(7, "events", "private"));
    inbox.deliver(7, "events", "committed");
    assert!(!batch.push(7, "events", "overflow"));
    assert!(inbox.is_closed());
    batch.commit();
    inbox.close();
    assert_eq!(
        inbox.poll().unwrap_err().kind(),
        NotificationFailureKind::Backpressure
    );
    assert_eq!(
        inbox.wait(Duration::ZERO).unwrap_err().kind(),
        NotificationFailureKind::Backpressure
    );
    assert_eq!(inbox.memory.used(), 0);
}

#[test]
fn byte_admission_covers_capacity_and_prepared_buffers_before_payload_copy() {
    for prepared in [false, true] {
        let inbox = inbox(NotificationSubscriptionOptions {
            max_queued_bytes: 1_024,
            ..options()
        });
        let payload = "secret".repeat(1_000);
        if prepared {
            let mut batch = inbox.prepare();
            assert!(!batch.push(7, "events", &payload));
            batch.commit();
        } else {
            inbox.deliver(7, "events", &payload);
        }
        let error = inbox.poll().unwrap_err();
        assert_eq!(error.kind(), NotificationFailureKind::Backpressure);
        assert!(inbox.memory.peak() <= 1_024);
        assert_eq!(inbox.memory.used(), 0);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
}

#[test]
fn closing_during_preparation_discards_the_private_values() {
    let inbox = inbox(options());
    let mut batch = inbox.prepare();
    assert!(batch.push(7, "events", "private"));
    inbox.close();
    batch.commit();
    assert_eq!(inbox.poll().unwrap(), Poll::Ready(None));
    assert_eq!(inbox.memory.used(), 0);
}

#[test]
fn receive_timeout_keeps_registration_and_close_wakes_waiters() {
    for timeout in [Duration::from_secs(30), Duration::MAX] {
        let inbox = inbox(options());
        assert_eq!(
            inbox.wait(Duration::ZERO).unwrap(),
            NotificationWait::TimedOut
        );
        assert!(!inbox.is_closed());
        inbox.deliver(7, "events", "after timeout");
        take(&inbox, 1, "after timeout");
        let ready = Arc::new(std::sync::Barrier::new(2));
        let waiter = {
            let inbox = inbox.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                inbox.wait(timeout).unwrap()
            })
        };
        ready.wait();
        inbox.close();
        assert_eq!(waiter.join().unwrap(), NotificationWait::Closed);
    }
}

#[test]
fn maximum_receive_timeout_preserves_events_and_terminal_errors() {
    let inbox = inbox(options());
    inbox.deliver(7, "events", "committed");
    let NotificationWait::Event(NotificationEvent::Notification {
        sequence,
        notification,
        ..
    }) = inbox.wait(Duration::MAX).unwrap()
    else {
        panic!("the queued event must remain available")
    };
    assert_eq!(sequence, 1);
    assert_eq!(notification.payload, "committed");
    inbox.fail(NotificationSubscriptionError::new(
        NotificationFailureKind::SourceUnavailable,
    ));
    assert_eq!(
        inbox.wait(Duration::MAX).unwrap_err().kind(),
        NotificationFailureKind::SourceUnavailable
    );
}

#[test]
fn terminal_source_failure_is_not_consumed_by_one_receiver() {
    let inbox = inbox(options());
    inbox.deliver(7, "events", "queued secret");
    inbox.fail(NotificationSubscriptionError::with_source(
        NotificationFailureKind::SourceUnavailable,
        std::io::Error::other("private database path"),
    ));
    for _ in 0..2 {
        let error = inbox.poll().unwrap_err();
        assert_eq!(error.code(), "NOTIFICATION_SOURCE_UNAVAILABLE");
        assert!(!format!("{error:?} {error}").contains("private"));
        assert_eq!(
            error.original_error().unwrap().to_string(),
            "private database path"
        );
    }
    assert_eq!(inbox.memory.used(), 0);
}

#[test]
fn exact_u64_sequence_never_wraps_or_loses_integer_precision() {
    let inbox = inbox(options());
    inbox.state.lock().next_sequence = Some(u64::MAX - 1);
    inbox.deliver(7, "events", "before maximum");
    inbox.deliver(7, "events", "maximum");
    take(&inbox, u64::MAX - 1, "before maximum");
    take(&inbox, u64::MAX, "maximum");
    assert_eq!(
        inbox.poll().unwrap_err().kind(),
        NotificationFailureKind::SequenceExhausted
    );
    assert_eq!(inbox.memory.used(), 0);
}
