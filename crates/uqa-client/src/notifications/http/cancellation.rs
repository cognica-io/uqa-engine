//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;
use tokio::sync::watch;

/// A monotonic, async-waking cancellation signal for an HTTP subscription's whole lifetime. It is independent of SQL and cannot be reset or used to restore cancelled authority.
#[derive(Clone)]
pub struct NotificationCancellation {
    signal: watch::Sender<bool>,
}

impl NotificationCancellation {
    pub fn new() -> Self {
        Self {
            signal: watch::channel(false).0,
        }
    }

    pub fn cancel(&self) {
        self.signal.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.signal.borrow()
    }

    pub(super) async fn cancelled(&self) {
        let mut receiver = self.signal.subscribe();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for NotificationCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for NotificationCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotificationCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
