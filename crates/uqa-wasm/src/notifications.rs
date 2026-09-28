//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained browser listeners with one native Future and one runtime wake slot.

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
};

use parking_lot::Mutex;
use serde_json::{json, Value as JSON};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind as Kind};
use uqa_engine::{
    Engine, NotificationSubscription, NotificationSubscriptionError,
    NotificationSubscriptionOptions,
};

type Receive = Pin<
    Box<
        dyn Future<Output = Result<Option<NotificationEvent>, NotificationSubscriptionError>>
            + Send,
    >,
>;

static LISTENERS: Mutex<BTreeMap<i32, Listener>> = Mutex::new(BTreeMap::new());
static NEXT_ID: AtomicI32 = AtomicI32::new(1);

struct Listener {
    source: Arc<NotificationSubscription>,
    receive: Option<Receive>,
    failure: Option<NotificationSubscriptionError>,
}

impl Listener {
    fn poll(&mut self, waker: &Waker) -> JSON {
        if let Some(error) = &self.failure {
            return failure(error);
        }
        let receive = self.receive.get_or_insert_with(|| {
            let source = Arc::clone(&self.source);
            Box::pin(async move { source.next_event().await })
        });
        let result = receive.as_mut().poll(&mut Context::from_waker(waker));
        if result.is_ready() {
            self.receive = None;
        }
        match result {
            Poll::Pending => json!({ "pending": true }),
            Poll::Ready(Ok(event)) => json!({ "event": event.map(event_value) }),
            Poll::Ready(Err(error)) => {
                let value = failure(&error);
                self.failure = Some(error);
                value
            }
        }
    }

    fn stop(&mut self) -> JSON {
        self.source.stop_delivery();
        // Stop discards unread values without erasing a previously latched failure.
        if self.failure.is_none() {
            self.failure = self.source.poll().err();
        }
        self.receive = None;
        self.failure.as_ref().map_or(JSON::Null, failure)
    }
}

struct RuntimeWake(i32);

impl Wake for RuntimeWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        #[cfg(target_os = "emscripten")]
        unsafe {
            uqa_notification_wake(self.0);
        }
        // Native unit tests poll with their own observable waker.
        #[cfg(not(target_os = "emscripten"))]
        let _ = self.0;
    }
}

#[cfg(target_os = "emscripten")]
extern "C" {
    fn uqa_notification_wake(id: i32);
}

pub(super) fn register(engine: &Engine, args: &JSON) -> Result<JSON, String> {
    let (channels, options) =
        arguments(args).map_err(|_| Kind::InvalidRequest.code().to_owned())?;
    let names: Vec<_> = channels.iter().map(String::as_str).collect();
    let source = match engine.subscribe_notifications(&names, options) {
        Ok(source) => source,
        Err(error) => return Ok(failure(&error)),
    };
    let id = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1).filter(|next| *next > 0)
        })
        .map_err(|_| Kind::Capacity.code().to_owned())?;
    let identity = source.identity();
    let value = json!({ "id": id, "epoch": identity.epoch.to_string(), "requestId": identity.request_id.as_ref().map(ToString::to_string) });
    LISTENERS.lock().insert(
        id,
        Listener {
            source: Arc::new(source),
            receive: None,
            failure: None,
        },
    );
    Ok(value)
}

pub(super) fn dispatch(method: &str, args: &JSON) -> Result<JSON, String> {
    let invalid = || Kind::InvalidRequest.code().to_owned();
    let id = args
        .get("id")
        .and_then(JSON::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .filter(|id| *id > 0)
        .ok_or_else(invalid)?;
    if method == "notificationClose" {
        let mut listener = LISTENERS.lock().remove(&id).ok_or_else(invalid)?;
        let status = listener.stop();
        listener.source.close();
        return Ok(status);
    }
    let mut listeners = LISTENERS.lock();
    let listener = listeners.get_mut(&id).ok_or_else(invalid)?;
    match method {
        "notificationNext" => Ok(listener.poll(&Waker::from(Arc::new(RuntimeWake(id))))),
        "notificationStop" => Ok(listener.stop()),
        "notificationStatus" => Ok(json!({ "closed": listener.source.is_closed() })),
        _ => Err(invalid()),
    }
}

fn arguments(args: &JSON) -> Result<(Vec<String>, NotificationSubscriptionOptions), String> {
    let limits = args.get("limits").ok_or("missing limits")?;
    let options = NotificationSubscriptionOptions {
        max_active_subscriptions: crate::arguments::req_usize(limits, "maxActiveSubscriptions")?,
        max_channels: crate::arguments::req_usize(limits, "maxChannels")?,
        max_queued_notifications: crate::arguments::req_usize(limits, "maxQueuedNotifications")?,
        max_queued_bytes: crate::arguments::req_usize(limits, "maxQueuedBytes")?,
        max_registry_entries_per_poll: crate::arguments::req_usize(
            limits,
            "maxRegistryEntriesPerPoll",
        )?,
    };
    let channels = crate::arguments::req_str_list(args, "channels")?;
    Ok((channels, options))
}

fn failure(error: &NotificationSubscriptionError) -> JSON {
    json!({ "failure": { "code": error.code(), "diagnostic": error.original_error().map(ToString::to_string) } })
}

fn event_value(event: NotificationEvent) -> JSON {
    let (kind, identity, sequence, notification, cause) = match event {
        NotificationEvent::Notification {
            identity,
            sequence,
            notification,
        } => (
            "notification",
            identity,
            Some(sequence),
            Some(notification),
            None,
        ),
        NotificationEvent::ResyncRequired { identity, cause } => {
            ("resync_required", identity, None, None, Some(cause.code()))
        }
        NotificationEvent::Reconnected { identity } => ("reconnected", identity, None, None, None),
    };
    json!({
        "kind": kind, "epoch": identity.epoch.to_string(), "requestId": identity.request_id.map(|value| value.to_string()),
        // Decimal text crosses JSON without passing through a JavaScript Number.
        "sequence": sequence.map(|value| value.to_string()),
        "processId": notification.as_ref().map(|value| value.process_id),
        "channel": notification.as_ref().map(|value| value.channel.as_str()),
        "payload": notification.as_ref().map(|value| value.payload.as_str()), "cause": cause,
    })
}

#[cfg(test)]
mod tests;
