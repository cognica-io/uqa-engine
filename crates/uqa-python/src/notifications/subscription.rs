//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python owns one native subscription and joins cancellation before explicit close returns.

use pyo3::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;
use uqa_client::notifications::{HttpNotificationSubscription, NotificationCancellation};
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind, NotificationIdentity};
use uqa_engine::NotificationSubscription;

use super::{errors::Failure, events::PyNotificationEvent};

pub(super) enum Source {
    Direct(NotificationSubscription),
    Http {
        subscription: AsyncMutex<HttpNotificationSubscription>,
        cancellation: NotificationCancellation,
    },
}

impl Source {
    fn stop_delivery(&self) {
        match self {
            Self::Direct(subscription) => subscription.stop_delivery(),
            Self::Http { cancellation, .. } => cancellation.cancel(),
        }
    }

    fn close(&self, runtime: &Runtime) -> Result<(), Failure> {
        self.stop_delivery();
        match self {
            Self::Direct(subscription) => {
                subscription.close();
                Ok(())
            }
            Self::Http { subscription, .. } => runtime
                .block_on(async { subscription.lock().await.close().await })
                .map_err(Failure::Http),
        }
    }
}

struct OwnedSubscription {
    source: Option<Source>,
    runtime: &'static Runtime,
    identity: Mutex<NotificationIdentity>,
    receiving: AtomicBool,
    stopping: AtomicBool,
    closed: AtomicBool,
}

impl OwnedSubscription {
    async fn next(&self) -> Result<Option<NotificationEvent>, Failure> {
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
            return Err(Failure::Local(NotificationFailureKind::InvalidRequest));
        }
        let _receiving = Receiving(&self.receiving);
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let result = match self.source.as_ref().expect("live subscription source") {
            Source::Direct(subscription) => {
                subscription.next_event().await.map_err(Failure::Direct)
            }
            Source::Http { subscription, .. } => subscription
                .lock()
                .await
                .next_event()
                .await
                .map_err(Failure::Http),
        };
        let result = match result {
            Err(Failure::Http(error))
                if self.stopping.load(Ordering::Acquire)
                    && error.kind() == NotificationFailureKind::Cancelled =>
            {
                Ok(None)
            }
            result => result,
        };
        if !matches!(&result, Ok(Some(_))) {
            self.stopping.store(true, Ordering::Release);
        }
        let event = result?;
        if let Some(NotificationEvent::Reconnected { identity }) = &event {
            *self
                .identity
                .lock()
                .map_err(|_| Failure::Local(NotificationFailureKind::SourceUnavailable))? =
                identity.clone();
        }
        Ok(event)
    }

    fn stop_delivery(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(source) = &self.source {
            source.stop_delivery();
        }
    }

    fn close(&self) -> Result<(), Failure> {
        self.stop_delivery();
        self.source
            .as_ref()
            .expect("live subscription source")
            .close(self.runtime)?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for OwnedSubscription {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.stop_delivery();
            if !self.closed.load(Ordering::Acquire) {
                let runtime = self.runtime;
                // GC must not run synchronous provider destruction on an asyncio
                // worker. The moved source retains its original admission permit.
                drop(self.runtime.spawn_blocking(move || {
                    let _ = source.close(runtime);
                }));
            }
        }
    }
}

#[pyclass(name = "NotificationSubscription", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationSubscription {
    inner: Arc<OwnedSubscription>,
}

impl PyNotificationSubscription {
    pub(super) fn direct(
        subscription: NotificationSubscription,
        runtime: &'static Runtime,
    ) -> Self {
        let identity = subscription.identity().clone();
        Self::new(Source::Direct(subscription), identity, runtime)
    }

    pub(super) fn http(
        subscription: HttpNotificationSubscription,
        runtime: &'static Runtime,
    ) -> Self {
        let identity = subscription.identity().clone();
        let cancellation = subscription.cancellation();
        Self::new(
            Source::Http {
                subscription: AsyncMutex::new(subscription),
                cancellation,
            },
            identity,
            runtime,
        )
    }

    fn new(source: Source, identity: NotificationIdentity, runtime: &'static Runtime) -> Self {
        Self {
            inner: Arc::new(OwnedSubscription {
                source: Some(source),
                runtime,
                identity: Mutex::new(identity),
                receiving: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
        }
    }
}

impl Drop for PyNotificationSubscription {
    fn drop(&mut self) {
        // A native receive can retain the source after this Python owner dies.
        // Wake it now so its final owner can schedule the retained cleanup.
        self.inner.stop_delivery();
    }
}

#[pymethods]
impl PyNotificationSubscription {
    #[getter]
    fn epoch(&self) -> PyResult<String> {
        Ok(self
            .inner
            .identity
            .lock()
            .map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err("subscription identity unavailable")
            })?
            .epoch
            .to_string())
    }

    #[getter]
    fn request_id(&self) -> PyResult<Option<String>> {
        Ok(self
            .inner
            .identity
            .lock()
            .map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err("subscription identity unavailable")
            })?
            .request_id
            .as_ref()
            .map(ToString::to_string))
    }

    #[getter]
    fn is_closed(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
            || matches!(&self.inner.source, Some(Source::Direct(subscription)) if subscription.is_closed())
    }

    fn next_event(&self, py: Python<'_>) -> PyResult<Option<PyNotificationEvent>> {
        let inner = Arc::clone(&self.inner);
        py.detach(move || inner.runtime.block_on(inner.next()))
            .map(|event| event.map(PyNotificationEvent))
            .map_err(|error| error.into_py(py))
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let inner = Arc::clone(&self.inner);
        py.detach(move || inner.close())
            .map_err(|error| error.into_py(py))
    }

    fn _stop_delivery(&self) {
        self.inner.stop_delivery();
    }

    fn _next_event_future(
        &self,
        py: Python<'_>,
        event_loop: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let future = event_loop.call_method0("create_future")?;
        let weakref = py.import("weakref")?;
        let weak_future = weakref.call_method1("ref", (&future,))?.unbind();
        let weak_loop = weakref.call_method1("ref", (event_loop,))?.unbind();
        let complete = py
            .import("uqa._notifications")?
            .getattr("_complete")?
            .unbind();
        let inner = Arc::clone(&self.inner);
        drop(self.inner.runtime.spawn(async move {
            let result = inner.next().await;
            // Only a completed receive needs Python execution. Idle receivers
            // retain a wake slot, not a Python or Tokio blocking-pool thread.
            drop(inner.runtime.spawn_blocking(move || {
                let delivered = Python::try_attach(|py| -> PyResult<()> {
                    let event_loop = weak_loop.bind(py).call0()?;
                    if event_loop.is_none() || weak_future.bind(py).call0()?.is_none() {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(
                            "notification consumer released",
                        ));
                    }
                    let converted = match result {
                        Ok(Some(event)) => {
                            Py::new(py, PyNotificationEvent(event)).map(Py::into_any)
                        }
                        Ok(None) => Ok(py.None()),
                        Err(error) => Err(error.into_py(py)),
                    };
                    let (event, error) = match converted {
                        Ok(event) => (event, py.None()),
                        Err(error) => (py.None(), error.into_value(py).into_any()),
                    };
                    event_loop.call_method1(
                        "call_soon_threadsafe",
                        (complete, weak_future, event, error),
                    )?;
                    Ok(())
                })
                .is_some_and(|result| result.is_ok());
                if !delivered {
                    let _ = inner.close();
                }
            }));
        }));
        Ok(future.unbind())
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<PyNotificationEvent>> {
        self.next_event(py)
    }
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __exit__(
        &self,
        py: Python<'_>,
        _kind: &Bound<'_, PyAny>,
        _value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        self.close(py)?;
        Ok(false)
    }
    fn __repr__(&self) -> &'static str {
        if self.is_closed() {
            "NotificationSubscription(closed)"
        } else {
            "NotificationSubscription(open)"
        }
    }
}
