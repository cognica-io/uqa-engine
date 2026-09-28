//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable Python failures keep private diagnostics behind explicit inspection.

use pyo3::{exceptions::PyRuntimeError, prelude::*};
use uqa_client::notifications::HttpNotificationError;
use uqa_core::notifications::NotificationFailureKind;
use uqa_engine::NotificationSubscriptionError;

pyo3::create_exception!(_uqa, NotificationError, PyRuntimeError);

#[derive(Clone)]
pub(super) enum Failure {
    Direct(NotificationSubscriptionError),
    Http(HttpNotificationError),
    Local(NotificationFailureKind),
}

impl Failure {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Direct(error) => error.code(),
            Self::Http(error) => error.code(),
            Self::Local(kind) => kind.code(),
        }
    }

    pub(super) fn into_py(self, py: Python<'_>) -> PyErr {
        let code = self.code();
        let error = NotificationError::new_err(code);
        if let Err(attribute_error) = error.value(py).setattr("code", code).and_then(|()| {
            error
                .value(py)
                .setattr("failure", Py::new(py, PyNotificationFailure(self))?)
        }) {
            return attribute_error;
        }
        error
    }
}

pub(super) fn invalid(py: Python<'_>) -> PyErr {
    Failure::Local(NotificationFailureKind::InvalidRequest).into_py(py)
}

#[pyfunction]
pub(crate) fn _invalid_notification(py: Python<'_>) -> Py<PyAny> {
    invalid(py).into_value(py).into_any()
}

#[pyclass(name = "NotificationFailure", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationFailure(Failure);

#[pymethods]
impl PyNotificationFailure {
    #[getter]
    fn code(&self) -> &'static str {
        self.0.code()
    }

    #[getter]
    fn original_failure(&self) -> Option<Self> {
        match &self.0 {
            Failure::Http(error) => error
                .original_failure()
                .cloned()
                .map(|value| Self(Failure::Http(value))),
            _ => None,
        }
    }

    #[getter]
    fn last_attempt_failure(&self) -> Option<Self> {
        match &self.0 {
            Failure::Http(error) => error
                .last_attempt_failure()
                .cloned()
                .map(|value| Self(Failure::Http(value))),
            _ => None,
        }
    }

    #[getter]
    fn http_status(&self) -> Option<u16> {
        match &self.0 {
            Failure::Http(error) => error.http_status(),
            _ => None,
        }
    }

    #[getter]
    fn diagnostic(&self) -> Option<String> {
        match &self.0 {
            Failure::Direct(error) => error.original_error().map(ToString::to_string),
            Failure::Http(error) => error
                .transport_error()
                .map(ToString::to_string)
                .or_else(|| error.server_message().map(str::to_owned))
                .or_else(|| error.protocol_error().map(|value| value.to_string())),
            Failure::Local(_) => None,
        }
    }

    fn __repr__(&self) -> String {
        format!("NotificationFailure(code={:?})", self.code())
    }
}
