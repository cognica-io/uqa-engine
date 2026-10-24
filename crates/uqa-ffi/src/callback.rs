//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation-scoped diagnostics and admission; no TLS borrow crosses a host callback.

use std::{cell::RefCell, ffi::c_char, sync::Arc};
use uqa_core::memory::MemoryBudget;

use crate::{
    abi::{UQAStatus, UQA_ERROR_BUSY, UQA_ERROR_PANIC},
    boundary::{self, Result},
    error::{self, CurrentError, RetainedError, UQAError},
    session::Session,
};

struct Frame {
    session: Option<Arc<Session>>,
    memory: MemoryBudget,
    failure: RetainedError,
    helper_error: Option<CurrentError>,
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
        Some(frame)
            if frame
                .session
                .as_ref()
                .is_some_and(|active| std::ptr::eq(active.as_ref(), session)) =>
        {
            Ok(true)
        }
        Some(_) => Err(UQAError::new(
            UQA_ERROR_BUSY,
            "value belongs to another callback session",
        )),
    })
}

pub(crate) fn record_panic() {
    let _ = inspect(|frame| {
        if let Some(frame) = frame {
            if let Some(session) = &frame.session {
                session.poison();
            }
            let _ = frame.failure.set(UQAError::new(
                UQA_ERROR_PANIC,
                "panic in a C callback helper",
            ));
        }
        Ok(())
    });
}

/// Move a helper's diagnostic out of ordinary TLS before a later helper clears
/// it. It becomes a callback failure only when the callback returns its status.
pub(crate) fn retain_helper_error(error: CurrentError) {
    let _ = FRAME.try_with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            if let Some(frame) = slot.as_mut() {
                frame.helper_error = Some(error);
            }
        }
    });
}

struct Scope;

impl Scope {
    fn enter(
        session: Option<&Arc<Session>>,
        memory: &MemoryBudget,
        failure: &RetainedError,
    ) -> Result<Self> {
        reject_reentry()?;
        error::clear();
        FRAME
            .try_with(|frame| {
                let mut frame = frame
                    .try_borrow_mut()
                    .map_err(|_| UQAError::state("callback scope is borrowed"))?;
                *frame = Some(Frame {
                    session: session.cloned(),
                    memory: memory.clone(),
                    failure: Arc::clone(failure),
                    helper_error: None,
                });
                Ok(Self)
            })
            .unwrap_or_else(|_| Err(UQAError::state("callback thread is exiting")))
    }

    fn finish(self) -> Option<CurrentError> {
        error::clear();
        let error = FRAME
            .try_with(|slot| slot.try_borrow_mut().ok()?.take()?.helper_error)
            .ok()
            .flatten();
        drop(self);
        error
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
    session: Option<&'a Arc<Session>>,
    memory: &'a MemoryBudget,
    pub(crate) failure: RetainedError,
}

impl<'a> Callbacks<'a> {
    pub(crate) fn new(session: &'a Arc<Session>) -> Result<Self> {
        Ok(Self {
            session: Some(session),
            memory: &session.memory,
            failure: error::retained_slot(&session.memory)?,
        })
    }

    /// Cleanup uses a slot admitted before the host resource was created. It
    /// remains callable after the creating session closes or its allowance fills.
    pub(crate) fn retained(
        session: Option<&'a Arc<Session>>,
        memory: &'a MemoryBudget,
        failure: RetainedError,
    ) -> Self {
        Self {
            session,
            memory,
            failure,
        }
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
        let scope = match Scope::enter(self.session, self.memory, &self.failure) {
            Ok(scope) => scope,
            Err(error) => return self.fail(error),
        };
        let status = invoke();
        let helper_error = scope.finish();
        if let Some(failure) = self.failure.get() {
            return failure.status;
        }
        if status < 0 {
            return self.fail(
                helper_error
                    .filter(|error| error.get().is_some_and(|error| error.status == status))
                    .map_or_else(
                        || UQAError::new(status, "C callback failed without a diagnostic"),
                        |error| error.into_owned(self.memory),
                    ),
            );
        }
        status
    }
}

pub(crate) fn check_scope(failure: &RetainedError) -> Result<()> {
    inspect(|frame| match frame {
        Some(frame) if Arc::ptr_eq(&frame.failure, failure) => Ok(()),
        _ => Err(UQAError::state("callback object is outside its invocation")),
    })
}

pub(crate) fn fail(status: UQAStatus, message: &str) -> Result<UQAStatus> {
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
                .set(UQAError::callback(status, message, &frame.memory));
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
