//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A Python registration retains independent cancellation before a worker starts.

use super::{errors::Failure, subscription::PyNotificationSubscription};
use pyo3::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::runtime::Runtime;
use uqa_client::{
    notifications::{HttpNotificationOptions, NotificationCancellation},
    HttpEngine,
};
use uqa_core::{notifications::NotificationFailureKind, CancellationToken};
use uqa_engine::{Engine, NotificationSubscriptionPermit};

pub(super) enum Target {
    Direct(Arc<Engine>, NotificationSubscriptionPermit),
    Http(Arc<HttpEngine>, HttpNotificationOptions),
}

#[pyclass(name = "_NotificationRegistration", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationRegistration {
    target: Mutex<Option<(Target, Vec<String>)>>,
    runtime: &'static Runtime,
    direct_cancel: CancellationToken,
    http_cancel: NotificationCancellation,
    ready: AtomicBool,
}

impl PyNotificationRegistration {
    pub(super) fn new(target: Target, channels: Vec<String>) -> PyResult<Self> {
        Ok(Self {
            target: Mutex::new(Some((target, channels))),
            runtime: crate::http_engine::http_runtime()?,
            direct_cancel: CancellationToken::new(),
            http_cancel: NotificationCancellation::new(),
            ready: AtomicBool::new(false),
        })
    }
}

#[pymethods]
impl PyNotificationRegistration {
    fn cancel(&self) {
        self.direct_cancel.cancel();
        self.http_cancel.cancel();
    }

    pub(super) fn run(&self, py: Python<'_>) -> PyResult<PyNotificationSubscription> {
        let target = self
            .target
            .lock()
            .map_err(|_| Failure::Local(NotificationFailureKind::SourceUnavailable).into_py(py))?
            .take()
            .ok_or_else(|| super::errors::invalid(py))?;
        let direct_cancel = self.direct_cancel.clone();
        let http_cancel = self.http_cancel.clone();
        let runtime = self.runtime;
        let result = py.detach(move || {
            let (target, channels) = target;
            if direct_cancel.is_cancelled() {
                return Err(Failure::Local(NotificationFailureKind::Cancelled));
            }
            let channels: Vec<_> = channels.iter().map(String::as_str).collect();
            match target {
                Target::Direct(engine, permit) => engine
                    .subscribe_notifications_with_permit(&channels, permit, &direct_cancel)
                    .map(|subscription| PyNotificationSubscription::direct(subscription, runtime))
                    .map_err(Failure::Direct),
                Target::Http(engine, options) => runtime
                    .block_on(engine.subscribe_notifications_with_cancellation(
                        &channels,
                        options,
                        &http_cancel,
                    ))
                    .map(|subscription| PyNotificationSubscription::http(subscription, runtime))
                    .map_err(Failure::Http),
            }
        });
        if result.is_ok() {
            self.ready.store(true, Ordering::Release);
        }
        result.map_err(|error| error.into_py(py))
    }
}

impl Drop for PyNotificationRegistration {
    fn drop(&mut self) {
        if !self.ready.load(Ordering::Acquire) {
            self.cancel();
        }
        if let Ok(target) = self.target.get_mut() {
            if let Some(target) = target.take() {
                drop(self.runtime.spawn_blocking(move || drop(target)));
            }
        }
    }
}
