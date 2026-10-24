//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation-scoped diagnostics and admission; no TLS borrow crosses a host callback.

use std::{cell::RefCell, ffi::c_char, sync::Arc};

use crate::{
    abi::{UQAStatus, UQA_ERROR_BUSY, UQA_ERROR_PANIC},
    boundary::{self, Result},
    error::{self, RetainedError, UQAError},
    session::Session,
};

struct Frame {
    session: Arc<Session>,
    failure: RetainedError,
}

thread_local! {
    static FRAME: RefCell<Option<Frame>> = const { RefCell::new(None) };
}

fn inspect<T>(read: impl FnOnce(Option<&Frame>) -> Result<T>) -> Result<T> {
    FRAME
        .try_with(|frame| {
            let frame = frame
                .try_borrow()
                .map_err(|_| UQAError::state("callback scope is borrowed"))?;
            read(frame.as_ref())
        })
        .unwrap_or_else(|_| Err(UQAError::state("callback thread is exiting")))
}

pub(crate) fn reject_reentry() -> Result<()> {
    inspect(|frame| {
        if frame.is_some() {
            Err(UQAError::new(
                UQA_ERROR_BUSY,
                "Engine reentry from a C callback is prohibited",
            ))
        } else {
            Ok(())
        }
    })
}

pub(crate) fn uses_value_scope(session: &Session) -> Result<bool> {
    inspect(|frame| match frame {
        None => Ok(false),
        Some(frame) if std::ptr::eq(frame.session.as_ref(), session) => Ok(true),
        Some(_) => Err(UQAError::new(
            UQA_ERROR_BUSY,
            "value belongs to another callback session",
        )),
    })
}

pub(crate) fn record_panic() {
    let _ = inspect(|frame| {
        if let Some(frame) = frame {
            frame.session.poison();
            let _ = frame.failure.set(UQAError::new(
                UQA_ERROR_PANIC,
                "panic in a C callback helper",
            ));
        }
        Ok(())
    });
}

struct Scope;

impl Scope {
    fn enter(session: &Arc<Session>, failure: &RetainedError) -> Result<Self> {
        reject_reentry()?;
        FRAME
            .try_with(|frame| {
                let mut frame = frame
                    .try_borrow_mut()
                    .map_err(|_| UQAError::state("callback scope is borrowed"))?;
                *frame = Some(Frame {
                    session: Arc::clone(session),
                    failure: Arc::clone(failure),
                });
                Ok(Self)
            })
            .unwrap_or_else(|_| Err(UQAError::state("callback thread is exiting")))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = FRAME.try_with(|frame| {
            if let Ok(mut frame) = frame.try_borrow_mut() {
                *frame = None;
            }
        });
    }
}

pub(crate) struct Callbacks<'a> {
    session: &'a Arc<Session>,
    pub(crate) failure: RetainedError,
}

impl<'a> Callbacks<'a> {
    pub(crate) fn new(session: &'a Arc<Session>) -> Result<Self> {
        Ok(Self {
            session,
            failure: error::retained_slot(&session.memory)?,
        })
    }

    pub(crate) fn fail(&self, error: UQAError) -> UQAStatus {
        let status = error.status;
        let _ = self.failure.set(error);
        self.failure.get().map_or(status, |error| error.status)
    }

    pub(crate) fn invoke(&self, invoke: impl FnOnce() -> UQAStatus) -> UQAStatus {
        if let Some(failure) = self.failure.get() {
            return failure.status;
        }
        let scope = match Scope::enter(self.session, &self.failure) {
            Ok(scope) => scope,
            Err(error) => return self.fail(error),
        };
        let status = invoke();
        drop(scope);
        if let Some(failure) = self.failure.get() {
            return failure.status;
        }
        if status < 0 {
            return self.fail(UQAError::new(
                status,
                "C callback failed without a diagnostic",
            ));
        }
        status
    }
}

fn fail(status: UQAStatus, message: &str) -> Result<UQAStatus> {
    if status >= 0 {
        return Err(UQAError::invalid(
            "callback failure status must be negative",
        ));
    }
    let failure = inspect(|frame| {
        let frame = frame.ok_or_else(|| UQAError::state("no active C callback"))?;
        if frame.failure.get().is_none() {
            let _ = frame
                .failure
                .set(UQAError::callback(status, message, &frame.session.memory));
        }
        Ok(Arc::clone(&frame.failure))
    })?;
    Ok(error::publish_retained(&failure))
}

ffi_fn!(uqa_callback_fail(status: UQAStatus, message: *const c_char) {
    fail(status, boundary::text(message)?)
});
ffi_fn!(uqa_callback_fail_n(status: UQAStatus, message: *const c_char, message_size: usize) {
    fail(status, boundary::text_n(message, message_size)?)
});
