//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact notification values with content-free diagnostic formatting.

use pyo3::prelude::*;
use uqa_core::notifications::NotificationEvent;

#[pyclass(name = "NotificationEvent", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationEvent(pub(super) NotificationEvent);

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::notifications::{NotificationIdentity, SQLNotification};

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
        self.0.kind()
    }

    #[getter]
    fn epoch(&self) -> String {
        self.0.identity().epoch.to_string()
    }

    #[getter]
    fn request_id(&self) -> Option<&str> {
        self.0
            .identity()
            .request_id
            .as_ref()
            .map(uqa_core::notifications::NotificationRequestId::as_str)
    }

    #[getter]
    fn sequence(&self) -> Option<u64> {
        self.0.sequence()
    }

    #[getter]
    fn process_id(&self) -> Option<i32> {
        self.0.notification().map(|value| value.process_id)
    }

    #[getter]
    fn channel(&self) -> Option<&str> {
        self.0.notification().map(|value| value.channel.as_str())
    }

    #[getter]
    fn payload(&self) -> Option<&str> {
        self.0.notification().map(|value| value.payload.as_str())
    }

    #[getter]
    fn cause(&self) -> Option<&'static str> {
        self.0
            .cause()
            .map(uqa_core::notifications::NotificationFailureKind::code)
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.0)
    }
}
