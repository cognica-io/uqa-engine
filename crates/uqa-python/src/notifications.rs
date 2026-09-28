//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python adapters share the original embedded and HTTP subscription owners.

mod errors;
mod events;
mod options;
mod registration;
mod subscription;

pub(crate) use errors::{NotificationError, PyNotificationFailure, _invalid_notification};
pub(crate) use events::PyNotificationEvent;
pub(crate) use options::{
    PyHttpNotificationOptions, PyNotificationOptions, PyNotificationRetryOptions,
};
pub(crate) use registration::PyNotificationRegistration;
pub(crate) use subscription::PyNotificationSubscription;

use crate::{PyEngine, PyHttpEngine};
use pyo3::{
    prelude::*,
    types::{PySequence, PyString},
};
use registration::Target;
use std::sync::Arc;
use uqa_core::notifications::NotificationFailureKind;

fn direct_registration(
    py: Python<'_>,
    engine: Arc<uqa_engine::Engine>,
    channels: Vec<String>,
    options: uqa_engine::NotificationSubscriptionOptions,
) -> PyResult<PyNotificationRegistration> {
    let permit = engine
        .reserve_notification_subscription(options, &uqa_core::CancellationToken::new())
        .map_err(|error| errors::Failure::Direct(error).into_py(py))?;
    PyNotificationRegistration::new(Target::Direct(engine, permit), channels)
}

fn channels_from_py(channels: &Bound<'_, PyAny>, maximum: usize) -> PyResult<Vec<String>> {
    let py = channels.py();
    if channels.is_instance_of::<PyString>() {
        return Err(errors::invalid(py));
    }
    let sequence = channels
        .cast::<PySequence>()
        .map_err(|_| errors::invalid(py))?;
    let count = sequence.len()?;
    if count == 0 || count > maximum {
        return Err(errors::invalid(py));
    }
    let mut channels = Vec::new();
    channels
        .try_reserve_exact(count)
        .map_err(|_| errors::Failure::Local(NotificationFailureKind::Capacity).into_py(py))?;
    for index in 0..count {
        let item = sequence.get_item(index)?;
        let item = item.cast::<PyString>().map_err(|_| errors::invalid(py))?;
        if item.len()? > 63 {
            return Err(errors::invalid(py));
        }
        let channel = item.to_cow().map_err(|_| errors::invalid(py))?;
        if channel.is_empty() || channel.len() > 63 || channel.contains('\0') {
            return Err(errors::invalid(py));
        }
        channels.push(channel.into_owned());
    }
    Ok(channels)
}

fn async_registration(
    py: Python<'_>,
    registration: PyNotificationRegistration,
) -> PyResult<Py<PyAny>> {
    Ok(py
        .import("uqa._notifications")?
        .getattr("_AsyncRegistration")?
        .call1((Py::new(py, registration)?,))?
        .unbind())
}

#[pymethods]
impl PyEngine {
    #[pyo3(signature = (channels, *, options))]
    fn subscribe_notifications(
        &self,
        py: Python<'_>,
        channels: &Bound<'_, PyAny>,
        options: PyRef<'_, PyNotificationOptions>,
    ) -> PyResult<PyNotificationSubscription> {
        let channels = channels_from_py(channels, options.0.max_channels)?;
        direct_registration(py, Arc::clone(self.inner()?), channels, options.0)?.run(py)
    }

    #[pyo3(signature = (channels, *, options))]
    fn subscribe_notifications_async(
        &self,
        py: Python<'_>,
        channels: &Bound<'_, PyAny>,
        options: PyRef<'_, PyNotificationOptions>,
    ) -> PyResult<Py<PyAny>> {
        let channels = channels_from_py(channels, options.0.max_channels)?;
        async_registration(
            py,
            direct_registration(py, Arc::clone(self.inner()?), channels, options.0)?,
        )
    }
}

#[pymethods]
impl PyHttpEngine {
    #[pyo3(signature = (channels, *, options))]
    fn subscribe_notifications(
        &self,
        py: Python<'_>,
        channels: &Bound<'_, PyAny>,
        options: PyRef<'_, PyHttpNotificationOptions>,
    ) -> PyResult<PyNotificationSubscription> {
        let channels = channels_from_py(channels, options.0.max_channels)?;
        PyNotificationRegistration::new(
            Target::Http(Arc::clone(&self.inner), options.0.clone()),
            channels,
        )?
        .run(py)
    }

    #[pyo3(signature = (channels, *, options))]
    fn subscribe_notifications_async(
        &self,
        py: Python<'_>,
        channels: &Bound<'_, PyAny>,
        options: PyRef<'_, PyHttpNotificationOptions>,
    ) -> PyResult<Py<PyAny>> {
        let channels = channels_from_py(channels, options.0.max_channels)?;
        async_registration(
            py,
            PyNotificationRegistration::new(
                Target::Http(Arc::clone(&self.inner), options.0.clone()),
                channels,
            )?,
        )
    }
}
