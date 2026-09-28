//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One owned HTTP subscription worker, its bounded inbox and explicit reconnect policy.

mod attempt;
mod cancellation;
mod error;
mod inbox;
mod options;
mod reconnect;
mod subscription;

pub use cancellation::NotificationCancellation;
pub use error::{HttpNotificationError, NotificationTimeoutStage};
pub use options::{HttpNotificationOptions, NotificationRetryOptions};
pub(crate) use subscription::subscribe;
pub use subscription::HttpNotificationSubscription;
