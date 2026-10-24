//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Thread-owned diagnostics. Inspection never replaces the current failure.

// Only exported getters and the current thread's borrowed diagnostic use raw C addresses.
#![allow(unsafe_code)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::Write;
use std::sync::{Arc, OnceLock};

use crate::abi::{
    UQAStatus, UQAStringView, UQA_COMPLETION_NONE, UQA_DONE, UQA_ERROR_BUSY,
    UQA_ERROR_INVALID_ARGUMENT, UQA_ERROR_LIMIT, UQA_ERROR_OUT_OF_MEMORY, UQA_ERROR_RANGE,
    UQA_ERROR_SQL, UQA_ERROR_STATE, UQA_OK,
};
use crate::boundary::{self, Result};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryReservation};
use uqa_core::notifications::NotificationFailureKind;
use uqa_sql::SQLError;

#[derive(Debug)]
pub struct UQAError {
    pub(crate) status: UQAStatus,
    message: Cow<'static, str>,
    native: Option<Box<SQLError>>,
    sqlstate: [u8; 5],
    sqlstate_present: bool,
    position: Option<u64>,
    pub(crate) completion: u32,
    pub(crate) statement_index: Option<u64>,
    cause: Option<RetainedError>,
    memory: Option<MemoryReservation>,
    notification: Option<NotificationFailureKind>,
    notification_detail: Option<Box<NotificationDiagnostic>>,
}

#[derive(Debug)]
struct NotificationDiagnostic {
    // Keep the native cause alive alongside its separately exposed private detail.
    _source: uqa_engine::NotificationSubscriptionError,
    detail: Cow<'static, str>,
}

impl UQAError {
    pub(crate) fn new(status: UQAStatus, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            message: message.into(),
            native: None,
            sqlstate: [0; 5],
            sqlstate_present: false,
            position: None,
            completion: UQA_COMPLETION_NONE,
            statement_index: None,
            cause: None,
            memory: None,
            notification: None,
            notification_detail: None,
        }
    }

    pub(crate) fn invalid(message: &'static str) -> Self {
        Self::new(UQA_ERROR_INVALID_ARGUMENT, message)
    }

    pub(crate) fn state(message: &'static str) -> Self {
        Self::new(UQA_ERROR_STATE, message)
    }

    pub(crate) fn callback(status: UQAStatus, message: &str, memory: &MemoryBudget) -> Self {
        let mut error = Self::new(status, "callback failed; diagnostic allocation failed");
        if let Ok(reservation) = memory.reserve(message.len()) {
            if let Ok(message) = boundary::copy_text(message) {
                error.message = Cow::Owned(message);
                error.memory = Some(reservation);
            }
        }
        error
    }

    pub(crate) fn with_cause(mut self, cause: &RetainedError) -> Self {
        if cause.get().is_some() {
            self.cause = Some(Arc::clone(cause));
        }
        self
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn is_notification_failure(&self) -> bool {
        self.notification.is_some()
    }

    pub(crate) fn display(status: UQAStatus, source: &impl std::fmt::Display) -> Self {
        let mut error = Self::new(status, "operation failed; diagnostic allocation failed");
        let mut message = DiagnosticWriter(String::new());
        if write!(message, "{source}").is_ok() {
            error.message = Cow::Owned(message.0);
        }
        error
    }
}

struct DiagnosticWriter(String);

impl Write for DiagnosticWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0
            .try_reserve(text.len())
            .map_err(|_| std::fmt::Error)?;
        self.0.push_str(text);
        Ok(())
    }
}

impl From<SQLError> for UQAError {
    fn from(native: SQLError) -> Self {
        let mut error = Self::new(
            UQA_ERROR_SQL,
            "SQL operation failed; diagnostic allocation failed",
        );
        if let Some(state) = native.sqlstate().filter(|state| state.len() == 5) {
            error.sqlstate.copy_from_slice(state.as_bytes());
            error.sqlstate_present = true;
        }
        error.position = native.position().map(u64::from);
        let mut message = DiagnosticWriter(String::new());
        if write!(message, "{}", native.primary()).is_ok() {
            error.message = Cow::Owned(message.0);
        }
        // Failure to retain richer fields still preserves status, SQLSTATE and position.
        error.native = boundary::boxed(native).ok();
        error
    }
}

impl From<uqa_core::memory::MemoryError> for UQAError {
    fn from(error: uqa_core::memory::MemoryError) -> Self {
        use uqa_core::memory::MemoryError;
        match error {
            MemoryError::Limit { .. } => {
                Self::new(UQA_ERROR_LIMIT, "FFI retention allowance exceeded")
            }
            MemoryError::SizeOverflow => Self::new(UQA_ERROR_RANGE, "FFI allocation size overflow"),
            MemoryError::Allocation(_) => {
                Self::new(UQA_ERROR_OUT_OF_MEMORY, "FFI allocation failed")
            }
        }
    }
}

impl From<uqa_core::ValueRetentionError> for UQAError {
    fn from(error: uqa_core::ValueRetentionError) -> Self {
        match error {
            uqa_core::ValueRetentionError::Memory(error) => error.into(),
            error => SQLError::from(error).into(),
        }
    }
}

impl From<uqa_engine::NotificationSubscriptionError> for UQAError {
    fn from(native: uqa_engine::NotificationSubscriptionError) -> Self {
        let mut error = Self::new(crate::abi::UQA_ERROR_NOTIFICATION, native.code());
        error.notification = Some(native.kind());
        if let Some(original) = native.original_error() {
            let mut detail = DiagnosticWriter(String::new());
            let detail = if write!(detail, "{original}").is_ok() {
                Cow::Owned(detail.0)
            } else {
                Cow::Borrowed("private diagnostic allocation failed")
            };
            error.notification_detail = boundary::boxed(NotificationDiagnostic {
                _source: native,
                detail,
            })
            .ok();
        }
        error
    }
}

fn notification_kind(kind: uqa_core::notifications::NotificationFailureKind) -> u32 {
    use crate::abi::{
        UQA_NOTIFICATION_FAILURE_AUTHENTICATION, UQA_NOTIFICATION_FAILURE_AUTHORITY_REVOKED,
        UQA_NOTIFICATION_FAILURE_BACKPRESSURE, UQA_NOTIFICATION_FAILURE_CANCELLED,
        UQA_NOTIFICATION_FAILURE_CAPACITY, UQA_NOTIFICATION_FAILURE_INVALID_REQUEST,
        UQA_NOTIFICATION_FAILURE_PROTOCOL, UQA_NOTIFICATION_FAILURE_SEQUENCE_EXHAUSTED,
        UQA_NOTIFICATION_FAILURE_SERVER_DRAINING, UQA_NOTIFICATION_FAILURE_SOURCE_UNAVAILABLE,
        UQA_NOTIFICATION_FAILURE_TIMEOUT, UQA_NOTIFICATION_FAILURE_TRANSPORT,
        UQA_NOTIFICATION_FAILURE_UNSUPPORTED,
    };
    use uqa_core::notifications::NotificationFailureKind;
    match kind {
        NotificationFailureKind::InvalidRequest => UQA_NOTIFICATION_FAILURE_INVALID_REQUEST,
        NotificationFailureKind::Authentication => UQA_NOTIFICATION_FAILURE_AUTHENTICATION,
        NotificationFailureKind::AuthorityRevoked => UQA_NOTIFICATION_FAILURE_AUTHORITY_REVOKED,
        NotificationFailureKind::Unsupported => UQA_NOTIFICATION_FAILURE_UNSUPPORTED,
        NotificationFailureKind::Capacity => UQA_NOTIFICATION_FAILURE_CAPACITY,
        NotificationFailureKind::Backpressure => UQA_NOTIFICATION_FAILURE_BACKPRESSURE,
        NotificationFailureKind::Protocol => UQA_NOTIFICATION_FAILURE_PROTOCOL,
        NotificationFailureKind::SourceUnavailable => UQA_NOTIFICATION_FAILURE_SOURCE_UNAVAILABLE,
        NotificationFailureKind::Transport => UQA_NOTIFICATION_FAILURE_TRANSPORT,
        NotificationFailureKind::Timeout => UQA_NOTIFICATION_FAILURE_TIMEOUT,
        NotificationFailureKind::ServerDraining => UQA_NOTIFICATION_FAILURE_SERVER_DRAINING,
        NotificationFailureKind::Cancelled => UQA_NOTIFICATION_FAILURE_CANCELLED,
        NotificationFailureKind::SequenceExhausted => UQA_NOTIFICATION_FAILURE_SEQUENCE_EXHAUSTED,
    }
}

/// Allocate a terminal diagnostic slot before admitting a retained operation. Re-publishing it
/// never copies diagnostic text or loses fields when a later allocation cannot be satisfied.
pub(crate) type RetainedError = Arc<Budgeted<OnceLock<UQAError>>>;

pub(crate) fn retained_slot(memory: &MemoryBudget) -> Result<RetainedError> {
    Ok(Budgeted::new(OnceLock::new(), memory.empty_reservation()).into_shared()?)
}

enum CurrentError {
    Owned(UQAError),
    Retained(RetainedError),
}

impl CurrentError {
    fn get(&self) -> Option<&UQAError> {
        match self {
            Self::Owned(error) => Some(error),
            Self::Retained(error) => error.get(),
        }
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CurrentError>> = const { RefCell::new(None) };
}

pub(crate) fn clear() {
    let _ = LAST_ERROR.try_with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            *slot = None;
        }
    });
}

pub(crate) fn publish(error: UQAError) -> UQAStatus {
    let status = error.status;
    replace(CurrentError::Owned(error));
    status
}

pub(crate) fn publish_retained(error: &RetainedError) -> UQAStatus {
    let Some(diagnostic) = error.get() else {
        return publish(UQAError::state(
            "retained diagnostic has not been populated",
        ));
    };
    let status = diagnostic.status;
    replace(CurrentError::Retained(Arc::clone(error)));
    status
}

fn replace(error: CurrentError) {
    let _ = LAST_ERROR.try_with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            *slot = Some(error);
        }
    });
}

fn inspect(
    pointer: *const UQAError,
    read: impl FnOnce(&UQAError) -> Result<UQAStatus>,
) -> UQAStatus {
    LAST_ERROR
        .try_with(|slot| {
            let Ok(slot) = slot.try_borrow() else {
                return UQA_ERROR_BUSY;
            };
            let mut current = slot.as_ref().and_then(CurrentError::get);
            while let Some(error) = current {
                if std::ptr::eq(error, pointer) {
                    return read(error).unwrap_or_else(|error| error.status);
                }
                current = error.cause.as_ref().and_then(|cause| cause.get());
            }
            UQA_ERROR_INVALID_ARGUMENT
        })
        .unwrap_or(UQA_ERROR_STATE)
}

/// # Safety
/// `out` must be a writable, aligned handle slot. The returned view belongs to this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uqa_error_get_last(out: *mut *const UQAError) -> UQAStatus {
    let Ok(out) = boundary::output(out) else {
        return UQA_ERROR_INVALID_ARGUMENT;
    };
    LAST_ERROR
        .try_with(|slot| {
            let Ok(slot) = slot.try_borrow() else {
                return UQA_ERROR_BUSY;
            };
            match slot.as_ref().and_then(CurrentError::get) {
                Some(error) => {
                    *out = error;
                    UQA_OK
                }
                None => UQA_DONE,
            }
        })
        .unwrap_or(UQA_ERROR_STATE)
}

macro_rules! diagnostic_getter {
    ($name:ident, $ty:ty, $field:expr) => {
        /// # Safety
        /// `error` must be the current thread's borrowed diagnostic and `out` a writable aligned slot.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(error: *const UQAError, out: *mut $ty) -> UQAStatus {
            let Ok(out) = boundary::output(out) else {
                return UQA_ERROR_INVALID_ARGUMENT;
            };
            inspect(error, |error| match ($field)(error) {
                Some(value) => {
                    *out = value;
                    Ok(UQA_OK)
                }
                None => Ok(UQA_DONE),
            })
        }
    };
}

diagnostic_getter!(uqa_error_get_status, UQAStatus, |e: &UQAError| Some(
    e.status
));
diagnostic_getter!(uqa_error_get_cause, *const UQAError, |e: &UQAError| e
    .cause
    .as_ref()
    .and_then(|cause| cause.get())
    .map(std::ptr::from_ref));
diagnostic_getter!(uqa_error_get_message, UQAStringView, |e: &UQAError| Some(
    e.message.as_ref().into()
));
diagnostic_getter!(uqa_error_get_sqlstate, UQAStringView, |e: &UQAError| e
    .sqlstate_present
    .then_some(UQAStringView {
        data: e.sqlstate.as_ptr().cast(),
        size: 5
    }));
diagnostic_getter!(uqa_error_get_detail, UQAStringView, |e: &UQAError| e
    .native
    .as_ref()
    .and_then(|n| n.detail())
    .or_else(|| e
        .notification_detail
        .as_ref()
        .map(|diagnostic| diagnostic.detail.as_ref()))
    .map(Into::into));
diagnostic_getter!(uqa_error_get_notification_kind, u32, |e: &UQAError| e
    .notification
    .map(notification_kind));
diagnostic_getter!(
    uqa_error_get_notification_code,
    UQAStringView,
    |e: &UQAError| e.notification.map(|kind| kind.code().into())
);
diagnostic_getter!(uqa_error_get_hint, UQAStringView, |e: &UQAError| e
    .native
    .as_ref()
    .and_then(|n| n.hint())
    .map(Into::into));
diagnostic_getter!(uqa_error_get_context, UQAStringView, |e: &UQAError| e
    .native
    .as_ref()
    .and_then(|n| n.context())
    .map(Into::into));
diagnostic_getter!(uqa_error_get_completion, u32, |e: &UQAError| Some(
    e.completion
));
diagnostic_getter!(uqa_error_get_statement_index, u64, |e: &UQAError| e
    .statement_index);
diagnostic_getter!(uqa_error_get_position, u64, |e: &UQAError| e.position);
