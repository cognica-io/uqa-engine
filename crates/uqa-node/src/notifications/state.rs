//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original registration, receive and provider-close ownership independent of a JS object.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex, MutexGuard,
};

use napi::{Env, Result, Task};
use tokio::sync::Notify;
use uqa_core::{notifications::NotificationFailureKind as Kind, CancellationToken};
use uqa_engine::{
    Engine, NotificationSubscription, NotificationSubscriptionError,
    NotificationSubscriptionOptions, NotificationSubscriptionPermit,
};

use super::{failure, NativeNotificationEvent, NativeNotificationIdentity};

struct Input {
    engine: Arc<Engine>,
    channels: Vec<String>,
    permit: NotificationSubscriptionPermit,
}

struct Registration {
    input: Option<Input>,
    submitted: bool,
    running: bool,
    source: Option<Arc<NotificationSubscription>>,
    failure: Option<NotificationSubscriptionError>,
}

pub(super) struct State {
    registration: Mutex<Registration>,
    registration_done: Condvar,
    cancellation: CancellationToken,
    stopping: AtomicBool,
    receiving: AtomicBool,
    cleanup_done: AtomicBool,
    cleanup_failed: AtomicBool,
    cleanup_notify: Notify,
}

impl State {
    pub(super) fn new(
        engine: Arc<Engine>,
        channels: Vec<String>,
        options: NotificationSubscriptionOptions,
    ) -> Result<Arc<Self>> {
        let cancellation = CancellationToken::new();
        let permit = engine
            .reserve_notification_subscription(options, &cancellation)
            .map_err(|error| failure(error.kind()))?;
        Ok(Arc::new(Self {
            registration: Mutex::new(Registration {
                input: Some(Input {
                    engine,
                    channels,
                    permit,
                }),
                submitted: false,
                running: false,
                source: None,
                failure: None,
            }),
            registration_done: Condvar::new(),
            cancellation,
            stopping: AtomicBool::new(false),
            receiving: AtomicBool::new(false),
            cleanup_done: AtomicBool::new(false),
            cleanup_failed: AtomicBool::new(false),
            cleanup_notify: Notify::new(),
        }))
    }

    fn registration(&self) -> MutexGuard<'_, Registration> {
        self.registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn submit(&self) -> Result<()> {
        let mut registration = self.registration();
        if registration.submitted || self.stopping.load(Ordering::Acquire) {
            return Err(failure(if self.stopping.load(Ordering::Acquire) {
                Kind::Cancelled
            } else {
                Kind::InvalidRequest
            }));
        }
        registration.submitted = true;
        Ok(())
    }

    fn register(&self) -> Result<NativeNotificationIdentity> {
        let input = {
            let mut registration = self.registration();
            let input = registration
                .input
                .take()
                .ok_or_else(|| failure(Kind::Cancelled))?;
            registration.running = true;
            input
        };
        let Input {
            engine,
            channels,
            permit,
        } = input;
        let channel_refs: Vec<_> = channels.iter().map(String::as_str).collect();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.subscribe_notifications_with_permit(&channel_refs, permit, &self.cancellation)
        }));
        drop(engine);
        let mut registration = self.registration();
        let result = match result {
            Ok(Ok(source)) => {
                let identity = NativeNotificationIdentity::from(source.identity());
                if self.stopping.load(Ordering::Acquire) {
                    source.stop_delivery();
                }
                registration.source = Some(Arc::new(source));
                Ok(identity)
            }
            Ok(Err(error)) => {
                let result = Err(failure(error.kind()));
                registration.failure = Some(error);
                result
            }
            Err(_) => Err(failure(Kind::SourceUnavailable)),
        };
        registration.running = false;
        self.registration_done.notify_all();
        result
    }

    pub(super) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.cancellation.cancel();
        let source = self.registration().source.clone();
        if let Some(source) = source {
            source.stop_delivery();
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
            || self
                .registration()
                .source
                .as_ref()
                .is_some_and(|source| source.is_closed())
    }

    pub(super) fn diagnostic(&self) -> Option<String> {
        self.registration()
            .failure
            .as_ref()?
            .original_error()
            .map(ToString::to_string)
    }

    pub(super) fn failure_code(&self) -> Option<&'static str> {
        self.registration()
            .failure
            .as_ref()
            .map(NotificationSubscriptionError::code)
    }

    pub(super) async fn next(&self) -> Result<Option<NativeNotificationEvent>> {
        struct Receiving<'a>(&'a AtomicBool);
        impl Drop for Receiving<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        if self
            .receiving
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(failure(Kind::InvalidRequest));
        }
        let _receiving = Receiving(&self.receiving);
        let source = self.registration().source.clone();
        let Some(source) = source else {
            if let Some(error) = self.registration().failure.as_ref() {
                return Err(failure(error.kind()));
            }
            return if self.stopping.load(Ordering::Acquire) {
                Ok(None)
            } else {
                Err(failure(Kind::InvalidRequest))
            };
        };
        source
            .next_event()
            .await
            .map(|value| value.map(Into::into))
            .map_err(|error| {
                let output = failure(error.kind());
                self.registration().failure = Some(error);
                output
            })
    }

    /// Called only by native asynchronous cleanup work, never a JS callback.
    pub(super) fn cleanup(&self) {
        self.stop();
        let (input, source) = {
            let mut registration = self.registration();
            while registration.running {
                registration = self
                    .registration_done
                    .wait(registration)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            (registration.input.take(), registration.source.take())
        };
        drop(input);
        if let Some(source) = source {
            // Delivery has stopped; poll cannot consume a queued event here.
            // Preserve a failure that won the inbox's close race before
            // releasing the original source and its private diagnostic cause.
            if let Err(error) = source.poll() {
                self.registration().failure = Some(error);
            }
            source.close();
        }
    }

    pub(super) fn finish_cleanup(&self, failed: bool) {
        self.cleanup_failed.store(failed, Ordering::Release);
        self.cleanup_done.store(true, Ordering::Release);
        self.cleanup_notify.notify_waiters();
    }

    pub(super) async fn closed(&self) -> Result<()> {
        loop {
            let notified = self.cleanup_notify.notified();
            if self.cleanup_done.load(Ordering::Acquire) {
                return if self.cleanup_failed.load(Ordering::Acquire) {
                    Err(failure(Kind::SourceUnavailable))
                } else {
                    Ok(())
                };
            }
            notified.await;
        }
    }
}

pub struct RegistrationTask(pub(super) Arc<State>);

impl Task for RegistrationTask {
    type Output = NativeNotificationIdentity;
    type JsValue = NativeNotificationIdentity;
    fn compute(&mut self) -> Result<Self::Output> {
        self.0.register()
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}
