//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::AtomicUsize;
use uqa_core::notifications::{NotificationIdentity, SQLNotification};

fn options() -> NotificationSubscriptionOptions {
    NotificationSubscriptionOptions {
        max_active_subscriptions: 2,
        max_channels: 2,
        max_queued_notifications: 2,
        max_queued_bytes: 65_536,
        max_registry_entries_per_poll: 2,
    }
}

struct CountWake(AtomicUsize);
impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn retained_future_wakes_after_query_owner_closes_and_releases_its_slot() {
    let engine = Engine::new();
    let source = Arc::new(
        engine
            .subscribe_notifications(&["events"], options())
            .unwrap(),
    );
    let mut listener = Listener {
        source: Arc::clone(&source),
        receive: None,
        failure: None,
    };
    let count = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&count));
    assert_eq!(listener.poll(&waker), json!({ "pending": true }));
    engine.sql("NOTIFY events, 'committed'", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    let event = listener.poll(&waker);
    assert_eq!(event["event"]["sequence"], "1");
    assert_eq!(event["event"]["payload"], "committed");
    assert!(listener.receive.is_none());
    assert_eq!(listener.poll(&waker), json!({ "pending": true }));
    listener.stop();
    source.close();
    assert_eq!(count.0.load(Ordering::SeqCst), 2);
    assert!(listener.receive.is_none());
    assert_eq!(listener.poll(&waker), json!({ "event": null }));
}

#[test]
fn terminal_failure_survives_stop_and_registry_cleanup() {
    let engine = Engine::new();
    let args = json!({ "channels": ["events"], "limits": {
        "maxActiveSubscriptions": 1, "maxChannels": 1,
        "maxQueuedNotifications": 1, "maxQueuedBytes": 65536,
        "maxRegistryEntriesPerPoll": 2,
    } });
    let identity = register(&engine, &args).unwrap();
    engine
        .sql("NOTIFY events, 'one'; NOTIFY events, 'two'", &[])
        .unwrap();
    let request = json!({ "id": identity["id"] });
    let stopped = dispatch("notificationStop", &request).unwrap();
    assert_eq!(stopped["failure"]["code"], "NOTIFICATION_BACKPRESSURE");
    assert_eq!(dispatch("notificationClose", &request).unwrap(), stopped);
    assert!(dispatch("notificationNext", &request).is_err());
    let replacement = register(&engine, &args).unwrap();
    dispatch("notificationClose", &json!({ "id": replacement["id"] })).unwrap();
}

#[test]
fn notification_json_preserves_full_width_and_control_values() {
    let identity = NotificationIdentity {
        epoch: "919108f7-52d1-4320-9bac-f847db4148a8".parse().unwrap(),
        request_id: None,
    };
    let value = event_value(NotificationEvent::Notification {
        identity: identity.clone(),
        sequence: u64::MAX,
        notification: SQLNotification {
            process_id: i32::MIN,
            channel: "混合😀".into(),
            payload: "\n\r\"\\한국어".into(),
        },
    });
    assert_eq!(value["sequence"], "18446744073709551615");
    assert_eq!(value["processId"], i32::MIN);
    assert_eq!(value["channel"], "混合😀");
    assert_eq!(value["payload"], "\n\r\"\\한국어");
    let gap = event_value(NotificationEvent::ResyncRequired {
        identity: identity.clone(),
        cause: Kind::SourceUnavailable,
    });
    assert_eq!(gap["cause"], Kind::SourceUnavailable.code());
    assert_eq!(gap["sequence"], JSON::Null);
    let ready = event_value(NotificationEvent::Reconnected { identity });
    assert_eq!(ready["kind"], "reconnected");
    assert_eq!(ready["cause"], JSON::Null);
}
