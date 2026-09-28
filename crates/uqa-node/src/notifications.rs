//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native listener adapters; JavaScript owns public iteration and cancellation.

mod cleanup;
mod events;
mod options;
mod promise;
mod state;

use std::sync::Arc;

use crate::Engine;
use napi::bindgen_prelude::{Array, AsyncTask, Env, Error, Result};
use napi_derive::napi;
use uqa_core::notifications::NotificationFailureKind;

use cleanup::Cleanup;
pub use events::{NativeNotificationEvent, NativeNotificationIdentity};
pub use options::NativeNotificationOptions;
pub use promise::NativePromise;
use state::{RegistrationTask, State};

fn failure(kind: NotificationFailureKind) -> Error {
    Error::from_reason(kind.code())
}

#[napi(js_name = "_NativeNotificationHandle")]
pub struct NativeNotificationHandle {
    state: Arc<State>,
    cleanup: Arc<Cleanup>,
}

#[napi]
impl Engine {
    /// Internal construction retains cancellation before registration is submitted.
    #[napi(js_name = "_notificationHandle", skip_typescript)]
    pub fn notification_handle(
        &self,
        env: Env,
        channels: Array<'_>,
        options: NativeNotificationOptions,
    ) -> Result<NativeNotificationHandle> {
        let options = options.validate()?;
        let channels = options::channels(channels, options.max_channels)?;
        let engine = Arc::clone(self.inner()?);
        let state = State::new(engine, channels, options)?;
        let cleanup = Cleanup::new(env, Arc::clone(&state))?;
        Ok(NativeNotificationHandle { state, cleanup })
    }
}

#[napi]
impl NativeNotificationHandle {
    #[napi(ts_return_type = "Promise<NativeNotificationIdentity>")]
    pub fn run(&self) -> Result<AsyncTask<RegistrationTask>> {
        self.state.submit()?;
        Ok(AsyncTask::new(RegistrationTask(Arc::clone(&self.state))))
    }

    #[napi(ts_return_type = "Promise<NativeNotificationEvent | null>")]
    pub fn next_event(&self, env: Env) -> Result<NativePromise<Option<NativeNotificationEvent>>> {
        let state = Arc::clone(&self.state);
        promise::run(env, async move { state.next().await })
    }

    #[napi]
    pub fn stop(&self) {
        self.state.stop();
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn close(&self, env: Env) -> Result<NativePromise<()>> {
        self.state.stop();
        self.cleanup.start()?;
        let state = Arc::clone(&self.state);
        promise::run(env, async move { state.closed().await })
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    #[napi]
    pub fn diagnostic(&self) -> Option<String> {
        self.state.diagnostic()
    }

    #[napi(getter)]
    pub fn failure_code(&self) -> Option<&'static str> {
        self.state.failure_code()
    }
}

impl Drop for NativeNotificationHandle {
    fn drop(&mut self) {
        self.state.stop();
        // The environment hook retains the cleanup owner even if queueing fails.
        let _ = self.cleanup.start();
    }
}
