//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reusable notification destinations own admitted native strings independently of the database.

use crate::{
    abi::{UQABytesView, UQAStringView, UQA_OK},
    boundary,
    error::UQAError,
};
use uqa_core::{
    memory::{MemoryBudget, MemoryReservation},
    notifications::{NotificationEvent, SQLNotification},
};

pub struct UQANotification {
    pub(crate) data: Option<Box<NotificationData>>,
}

pub(crate) struct NotificationData {
    native: Option<SQLNotification>,
    epoch: [u8; 16],
    sequence: u64,
    _memory: MemoryReservation,
}

impl NotificationData {
    pub(crate) fn prepare(
        value: &SQLNotification,
        budget: &MemoryBudget,
    ) -> boundary::Result<Box<Self>> {
        let mut memory = budget.reserve(size_of::<Self>())?;
        memory.grow(value.channel.capacity())?;
        memory.grow(value.payload.capacity())?;
        boundary::boxed(Self {
            native: None,
            epoch: [0; 16],
            sequence: 0,
            _memory: memory,
        })
    }

    pub(crate) fn finish(
        mut self: Box<Self>,
        event: NotificationEvent,
    ) -> boundary::Result<Box<Self>> {
        let NotificationEvent::Notification {
            identity,
            sequence,
            notification,
        } = event
        else {
            return Err(UQAError::state(
                "embedded subscription returned a reconnect event",
            ));
        };
        self.native = Some(notification);
        self.epoch = *identity.epoch.as_bytes();
        self.sequence = sequence;
        Ok(self)
    }

    fn native(&self) -> &SQLNotification {
        self.native
            .as_ref()
            .expect("populated notification owns its native payload")
    }
}

impl UQANotification {
    fn populated(&self) -> boundary::Result<&NotificationData> {
        self.data
            .as_deref()
            .ok_or_else(|| UQAError::state("notification has not received an event"))
    }
}

ffi_fn!(uqa_notification_create(out: *mut *mut UQANotification) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQANotification { data: None })?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_notification_destroy(notification: *mut *mut UQANotification) { boundary::release(notification) });
ffi_fn!(uqa_notification_get_channel(notification: *mut UQANotification, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    *out = boundary::input(notification)?.populated()?.native().channel.as_str().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_notification_get_payload(notification: *mut UQANotification, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    *out = boundary::input(notification)?.populated()?.native().payload.as_str().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_notification_get_sender_pid(notification: *mut UQANotification, out: *mut i32) {
    let out = boundary::output(out)?;
    *out = boundary::input(notification)?.populated()?.native().process_id;
    Ok(UQA_OK)
});
ffi_fn!(uqa_notification_get_epoch(notification: *mut UQANotification, out: *mut UQABytesView) {
    let out = boundary::output(out)?;
    let epoch = &boundary::input(notification)?.populated()?.epoch;
    *out = UQABytesView { data: epoch.as_ptr(), size: epoch.len() };
    Ok(UQA_OK)
});
ffi_fn!(uqa_notification_get_sequence(notification: *mut UQANotification, out: *mut u64) {
    let out = boundary::output(out)?;
    *out = boundary::input(notification)?.populated()?.sequence;
    Ok(UQA_OK)
});
