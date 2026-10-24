//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent native listeners, serialized receive leases and completed close.

mod options;
pub use options::{UQASQLNotificationOptions, UQASubscriptionOptions};

use crate::{
    abi::{UQAStatus, UQA_DONE, UQA_ERROR_BUSY, UQA_ERROR_PANIC, UQA_OK, UQA_TIMEOUT},
    boundary, callback,
    error::{self, RetainedError, UQAError},
    notification::{NotificationData, UQANotification},
    session::{Session, UQAEngine},
};
use parking_lot::{Condvar, Mutex};
use std::{
    ffi::c_char,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, OnceLock},
    time::Duration,
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};
use uqa_engine::{NotificationReceive, NotificationSubscription, NotificationSubscriptionOptions};

pub struct UQASubscription {
    inner: Arc<Budgeted<Subscription>>,
    _memory: MemoryReservation,
}

struct Subscription {
    native: OnceLock<NotificationSubscription>,
    memory: MemoryBudget,
    terminal: RetainedError,
    admission: Mutex<Admission>,
    released: Condvar,
}

#[derive(Default)]
struct Admission {
    active: bool,
    closing: bool,
}

impl Subscription {
    fn native(&self) -> &NotificationSubscription {
        self.native
            .get()
            .expect("published subscription has completed native registration")
    }

    fn enter(this: &Arc<Budgeted<Self>>) -> boundary::Result<Option<ReceiveLease>> {
        let mut admission = this.admission.lock();
        if admission.closing {
            return Ok(None);
        }
        if admission.active {
            return Err(UQAError::new(
                UQA_ERROR_BUSY,
                "subscription already has a receiver",
            ));
        }
        admission.active = true;
        Ok(Some(ReceiveLease(Arc::clone(this))))
    }

    fn receive(
        &self,
        timeout: Duration,
    ) -> boundary::Result<NotificationReceive<Box<NotificationData>>> {
        if let Ok(outcome) = catch_unwind(AssertUnwindSafe(|| {
            self.native().wait_with(timeout, |value| {
                NotificationData::prepare(value, &self.memory)
            })
        })) {
            outcome
        } else {
            self.native().stop_delivery();
            Err(UQAError::new(
                UQA_ERROR_PANIC,
                "panic while receiving a notification",
            ))
        }
    }

    fn close(&self) {
        let mut admission = self.admission.lock();
        admission.closing = true;
        drop(admission);
        self.native().stop_delivery();
        let mut admission = self.admission.lock();
        while admission.active {
            self.released.wait(&mut admission);
        }
        drop(admission);
        self.native().close();
    }
}

struct ReceiveLease(Arc<Budgeted<Subscription>>);
impl Drop for ReceiveLease {
    fn drop(&mut self) {
        self.0.admission.lock().active = false;
        self.0.released.notify_all();
    }
}

fn open(
    session: &Session,
    channels: &[&str],
    options: NotificationSubscriptionOptions,
) -> boundary::Result<*mut UQASubscription> {
    let inner = Budgeted::new(
        Subscription {
            native: OnceLock::new(),
            memory: session.memory.clone(),
            terminal: error::retained_slot(&session.memory)?,
            admission: Mutex::new(Admission::default()),
            released: Condvar::new(),
        },
        session.memory.empty_reservation(),
    )
    .into_shared()?;
    let handle = boundary::boxed(UQASubscription {
        inner,
        _memory: session.memory.reserve(size_of::<UQASubscription>())?,
    })?;
    let native = session.native.subscribe_notifications_with_cancellation(
        channels,
        options,
        &session.cancellation,
    )?;
    if handle.inner.native.set(native).is_err() {
        return Err(UQAError::state("subscription was already initialized"));
    }
    Ok(Box::into_raw(handle))
}

// The C caller keeps both channel arrays and all declared text spans valid throughout open.
#[allow(unsafe_code)]
unsafe fn channels<'a>(
    session: &Session,
    pointers: *const *const c_char,
    sizes: Option<*const usize>,
    count: usize,
) -> boundary::Result<BudgetedVec<&'a str>> {
    let pointers = boundary::span(pointers, count)?;
    let sizes = sizes
        .map(|sizes| boundary::span(sizes, count))
        .transpose()?;
    let mut channels = BudgetedVec::new(&session.memory);
    channels.reserve(count)?;
    for (index, pointer) in pointers.iter().enumerate() {
        channels.push(match sizes {
            Some(sizes) => boundary::text_n(*pointer, sizes[index])?,
            None => boundary::text(*pointer)?,
        })?;
    }
    Ok(channels)
}

ffi_fn!(uqa_subscription_open(engine: *mut UQAEngine, channel_names: *const *const c_char, count: usize, options: *const UQASubscriptionOptions, out: *mut *mut UQASubscription) {
    let out = boundary::new_handle(out)?;
    let options = options::subscription(options)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let channels = channels(session, channel_names, None, count)?;
    *out = open(session, &channels, options)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_subscription_open_n(engine: *mut UQAEngine, channel_names: *const *const c_char, channel_sizes: *const usize, count: usize, options: *const UQASubscriptionOptions, out: *mut *mut UQASubscription) {
    let out = boundary::new_handle(out)?;
    let options = options::subscription(options)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let channels = channels(session, channel_names, Some(channel_sizes), count)?;
    *out = open(session, &channels, options)?;
    Ok(UQA_OK)
});

// Clone the admitted owner's lease before the close operation can retire the outer C handle.
#[allow(unsafe_code)]
unsafe fn receive(
    subscription: *mut UQASubscription,
    destination: *mut UQANotification,
    timeout: Duration,
) -> boundary::Result<UQAStatus> {
    callback::reject_reentry()?;
    boundary::input(destination)?;
    let inner = Arc::clone(&boundary::input(subscription)?.inner);
    let Some(_lease) = Subscription::enter(&inner)? else {
        return Ok(UQA_DONE);
    };
    if inner.terminal.get().is_some() {
        return Ok(error::publish_retained(&inner.terminal));
    }
    match inner.receive(timeout) {
        Ok(NotificationReceive::Event { event, admitted }) => {
            boundary::input_mut(destination)?.data = Some(admitted.finish(event)?);
            Ok(UQA_OK)
        }
        Ok(NotificationReceive::TimedOut) => Ok(UQA_TIMEOUT),
        Ok(NotificationReceive::Closed) => Ok(UQA_DONE),
        Err(failure) if failure.is_notification_failure() || failure.status == UQA_ERROR_PANIC => {
            let _ = inner.terminal.set(failure);
            Ok(error::publish_retained(&inner.terminal))
        }
        Err(failure) => Err(failure),
    }
}

ffi_fn!(uqa_subscription_poll(subscription: *mut UQASubscription, notification: *mut UQANotification) {
    receive(subscription, notification, Duration::ZERO)
});
ffi_fn!(uqa_subscription_wait(subscription: *mut UQASubscription, timeout_ms: u64, notification: *mut UQANotification) {
    receive(subscription, notification, Duration::from_millis(timeout_ms))
});
ffi_fn!(uqa_subscription_close(subscription: *mut *mut UQASubscription) {
    if subscription.is_null() { return Ok(UQA_OK); }
    let pointer = *boundary::input(subscription)?;
    if pointer.is_null() { return Ok(UQA_OK); }
    callback::reject_reentry()?;
    let inner = Arc::clone(&boundary::input(pointer)?.inner);
    inner.close();
    boundary::release(subscription)
});

ffi_fn!(uqa_engine_configure_sql_notifications(engine: *mut UQAEngine, options: *const UQASQLNotificationOptions) {
    let options = options::sql(options)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let terminal = error::retained_slot(&session.memory)?;
    session.native.configure_sql_notifications(options)?;
    let _ = session.sql_notification_error.set(terminal);
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_poll_sql_notification(engine: *mut UQAEngine, notification: *mut UQANotification) {
    boundary::input(notification)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    match session.native.try_take_sql_notification_with(|value| NotificationData::prepare(value, &session.memory)) {
        Ok(NotificationReceive::Event { event, admitted }) => {
            boundary::input_mut(notification)?.data = Some(admitted.finish(event)?);
            Ok(UQA_OK)
        },
        Ok(NotificationReceive::TimedOut) => Ok(UQA_TIMEOUT),
        Ok(NotificationReceive::Closed) => Ok(UQA_DONE),
        Err(failure) => {
            if failure.is_notification_failure() {
                if let Some(terminal) = session.sql_notification_error.get() {
                    let _ = terminal.set(failure);
                    return Ok(error::publish_retained(terminal));
                }
            }
            Err(failure)
        },
    }
});
