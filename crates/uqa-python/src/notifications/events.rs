//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact notification values with content-free diagnostic formatting.

use pyo3::prelude::*;
use uqa_core::notifications::{NotificationEvent, NotificationIdentity, SQLNotification};

#[pyclass(name = "NotificationEvent", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationEvent(pub(super) NotificationEvent);

impl PyNotificationEvent {
    fn identity(&self) -> &NotificationIdentity {
        match &self.0 {
            NotificationEvent::Notification { identity, .. }
            | NotificationEvent::ResyncRequired { identity, .. }
            | NotificationEvent::Reconnected { identity } => identity,
        }
    }

    fn notification(&self) -> Option<&SQLNotification> {
        match &self.0 {
            NotificationEvent::Notification { notification, .. } => Some(notification),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_values_preserve_full_width_sequences_and_private_payloads() {
        Python::initialize();
        Python::attach(|py| {
            let event = PyNotificationEvent(NotificationEvent::Notification {
                identity: NotificationIdentity {
                    epoch: "7fb52b7f-bdca-4db2-9ee0-490f99857201".parse().unwrap(),
                    request_id: None,
                },
                sequence: u64::MAX,
                notification: SQLNotification {
                    process_id: i32::MIN,
                    channel: "private_channel".to_owned(),
                    payload: "opaque\n문자😀".to_owned(),
                },
            });
            let value = Py::new(py, event).unwrap();
            let value = value.bind(py);
            assert_eq!(
                value.getattr("sequence").unwrap().extract::<u64>().unwrap(),
                u64::MAX
            );
            assert_eq!(
                value
                    .getattr("sequence")
                    .unwrap()
                    .str()
                    .unwrap()
                    .to_string(),
                "18446744073709551615"
            );
            assert_eq!(
                value
                    .getattr("process_id")
                    .unwrap()
                    .extract::<i32>()
                    .unwrap(),
                i32::MIN
            );
            assert_eq!(
                value
                    .getattr("payload")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "opaque\n문자😀"
            );
            let diagnostic = value.repr().unwrap().to_string();
            assert!(!diagnostic.contains("private_channel"));
            assert!(!diagnostic.contains("opaque"));
        });
    }
}

#[pymethods]
impl PyNotificationEvent {
    #[getter]
    fn kind(&self) -> &'static str {
        match &self.0 {
            NotificationEvent::Notification { .. } => "notification",
            NotificationEvent::ResyncRequired { .. } => "resync_required",
            NotificationEvent::Reconnected { .. } => "reconnected",
        }
    }

    #[getter]
    fn epoch(&self) -> String {
        self.identity().epoch.to_string()
    }

    #[getter]
    fn request_id(&self) -> Option<&str> {
        self.identity()
            .request_id
            .as_ref()
            .map(uqa_core::notifications::NotificationRequestId::as_str)
    }

    #[getter]
    fn sequence(&self) -> Option<u64> {
        match self.0 {
            NotificationEvent::Notification { sequence, .. } => Some(sequence),
            _ => None,
        }
    }

    #[getter]
    fn process_id(&self) -> Option<i32> {
        self.notification().map(|value| value.process_id)
    }

    #[getter]
    fn channel(&self) -> Option<&str> {
        self.notification().map(|value| value.channel.as_str())
    }

    #[getter]
    fn payload(&self) -> Option<&str> {
        self.notification().map(|value| value.payload.as_str())
    }

    #[getter]
    fn cause(&self) -> Option<&'static str> {
        match self.0 {
            NotificationEvent::ResyncRequired { cause, .. } => Some(cause.code()),
            _ => None,
        }
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.0)
    }
}
