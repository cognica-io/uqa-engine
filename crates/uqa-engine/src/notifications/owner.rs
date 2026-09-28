//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! External hub ownership ends and joins polling before retained storage is released.

use super::{Arc, NotificationHub};

pub(crate) struct NotificationHubOwner {
    hub: Arc<NotificationHub>,
}

impl NotificationHubOwner {
    pub(super) fn new(hub: Arc<NotificationHub>) -> Self {
        Self { hub }
    }
}

impl Default for NotificationHubOwner {
    fn default() -> Self {
        Self::new(Arc::new(NotificationHub::default()))
    }
}

impl std::ops::Deref for NotificationHubOwner {
    type Target = NotificationHub;
    fn deref(&self) -> &Self::Target {
        &self.hub
    }
}

impl Drop for NotificationHubOwner {
    fn drop(&mut self) {
        #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
        if let Some(coordinator) = self
            .hub
            .cross
            .as_ref()
            .and_then(super::CrossProcessState::initialized_coordinator)
        {
            coordinator.shutdown();
        }
    }
}

#[cfg(all(test, any(windows, all(unix, not(target_os = "emscripten")))))]
mod tests;
