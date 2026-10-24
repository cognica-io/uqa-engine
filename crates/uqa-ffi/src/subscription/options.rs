//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Size/version-tagged C options. Read the prefix before forming a complete typed reference.

// C supplies readable storage for its declared size; references are formed only after size checks.
#![allow(unsafe_code)]

use crate::{abi::UQA_ERROR_RANGE, boundary, error::UQAError};
use uqa_engine::{NotificationSubscriptionOptions, SQLNotificationOptions};

#[repr(C)]
struct Header {
    struct_size: u32,
    version: u32,
}

#[repr(C)]
pub struct UQASubscriptionOptions {
    pub struct_size: u32,
    pub version: u32,
    pub max_active_subscriptions: u64,
    pub max_channels: u64,
    pub max_queued_notifications: u64,
    pub max_queued_bytes: u64,
    pub max_registry_entries_per_poll: u64,
}

#[repr(C)]
pub struct UQASQLNotificationOptions {
    pub struct_size: u32,
    pub version: u32,
    pub max_channels: u64,
    pub max_queued_notifications: u64,
    pub max_queued_bytes: u64,
    pub max_registry_entries_per_poll: u64,
}

unsafe fn read<'a, T>(pointer: *const T) -> boundary::Result<&'a T> {
    let header = boundary::input(pointer.cast::<Header>())?;
    let size = header.struct_size as usize;
    if header.version != 1 || size < size_of::<T>() {
        return Err(UQAError::invalid(
            "notification options require version 1 and every limit field",
        ));
    }
    let bytes = boundary::span(pointer.cast::<u8>(), size)?;
    if bytes[size_of::<T>()..].iter().any(|byte| *byte != 0) {
        return Err(UQAError::invalid(
            "unknown notification option bytes must be zero",
        ));
    }
    boundary::input(pointer)
}

fn positive(value: u64) -> boundary::Result<usize> {
    if value == 0 {
        return Err(UQAError::invalid("notification limits must be positive"));
    }
    usize::try_from(value).map_err(|_| {
        UQAError::new(
            UQA_ERROR_RANGE,
            "notification limit exceeds the platform range",
        )
    })
}

pub(super) unsafe fn subscription(
    pointer: *const UQASubscriptionOptions,
) -> boundary::Result<NotificationSubscriptionOptions> {
    let options = read(pointer)?;
    Ok(NotificationSubscriptionOptions {
        max_active_subscriptions: positive(options.max_active_subscriptions)?,
        max_channels: positive(options.max_channels)?,
        max_queued_notifications: positive(options.max_queued_notifications)?,
        max_queued_bytes: positive(options.max_queued_bytes)?,
        max_registry_entries_per_poll: positive(options.max_registry_entries_per_poll)?,
    })
}

pub(super) unsafe fn sql(
    pointer: *const UQASQLNotificationOptions,
) -> boundary::Result<SQLNotificationOptions> {
    let options = read(pointer)?;
    Ok(SQLNotificationOptions {
        max_channels: positive(options.max_channels)?,
        max_queued_notifications: positive(options.max_queued_notifications)?,
        max_queued_bytes: positive(options.max_queued_bytes)?,
        max_registry_entries_per_poll: positive(options.max_registry_entries_per_poll)?,
    })
}
