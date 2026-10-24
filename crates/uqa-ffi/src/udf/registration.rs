//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Registration handles own user data without retaining their creating session.

// The application synchronizes shared user data and keeps callback code loaded;
// native retirement admits destruction only after every execution lease is gone.
#![allow(unsafe_code)]

use super::UQAUDFOptions;
use crate::{
    abi::{UQAStatus, UQA_ERROR_BUSY, UQA_OK},
    boundary,
    callback::{self, Callbacks},
    error::{self, RetainedError, UQAError},
    session::Session,
};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryReservation};
use uqa_engine::{SQLFunctionOptions, SQLFunctionRegistration, SQLFunctionRegistrationError};

pub type UQAUserDataDestroyFn = unsafe extern "C" fn(*mut c_void) -> UQAStatus;
pub(super) type SharedData = Arc<Budgeted<UserData>>;

pub(super) struct UserData {
    pub(super) pointer: *mut c_void,
    destroy: Option<UQAUserDataDestroyFn>,
    owned: AtomicBool,
    session: Weak<Session>,
    memory: MemoryBudget,
    failure: RetainedError,
}
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

impl UserData {
    fn retire(&self) -> UQAStatus {
        if !self.owned.swap(false, Ordering::AcqRel) {
            return UQA_OK;
        }
        let Some(destroy) = self.destroy else {
            return UQA_OK;
        };
        let session = self.session.upgrade();
        let callbacks =
            Callbacks::retained(session.as_ref(), &self.memory, Arc::clone(&self.failure));
        let status = callbacks.invoke(|| unsafe { destroy(self.pointer) });
        if status > 0 {
            callbacks.fail(UQAError::invalid(
                "user-data destructor must return UQA_OK or a negative status",
            ))
        } else {
            status
        }
    }
}
impl Drop for UserData {
    fn drop(&mut self) {
        self.retire();
    }
}

pub struct UQAUDF {
    native: Option<SQLFunctionRegistration>,
    pub(super) data: SharedData,
    _memory: MemoryReservation,
}

impl UQAUDF {
    pub(super) fn prepare(
        session: &Arc<Session>,
        user_data: *mut c_void,
        destroy: Option<UQAUserDataDestroyFn>,
    ) -> boundary::Result<Box<Self>> {
        let memory = session.memory.reserve(size_of::<Self>())?;
        let failure = error::retained_slot(&session.memory)?;
        let data = Budgeted::new(
            UserData {
                pointer: user_data,
                destroy,
                owned: AtomicBool::new(false),
                session: Arc::downgrade(session),
                memory: session.memory.clone(),
                failure,
            },
            session.memory.empty_reservation(),
        )
        .into_shared()?;
        boundary::boxed(Self {
            native: None,
            data,
            _memory: memory,
        })
    }

    pub(super) fn publish(mut self: Box<Self>, native: SQLFunctionRegistration) -> *mut Self {
        self.native = Some(native);
        self.data.owned.store(true, Ordering::Release);
        Box::into_raw(self)
    }
}

pub(super) fn registration_error(error: SQLFunctionRegistrationError) -> UQAError {
    match error {
        SQLFunctionRegistrationError::Busy => UQAError::new(
            UQA_ERROR_BUSY,
            "shared function registry has active execution or retained resources",
        ),
        SQLFunctionRegistrationError::Invalid(error) => error.into(),
    }
}

pub(super) unsafe fn options(options: *mut UQAUDFOptions) -> boundary::Result<SQLFunctionOptions> {
    Ok(if options.is_null() {
        SQLFunctionOptions::default()
    } else {
        boundary::input(options)?.options
    })
}

ffi_fn!(uqa_udf_unregister(function: *mut *mut UQAUDF) {
    if function.is_null() { return Ok(UQA_OK); }
    let pointer = *boundary::input(function)?;
    if pointer.is_null() { return Ok(UQA_OK); }
    callback::reject_reentry()?;
    let handle = boundary::input_mut(pointer)?;
    if let Some(native) = &mut handle.native { native.unregister().map_err(registration_error)?; }
    let failure = Arc::clone(&handle.data.failure);
    let status = handle.data.retire();
    boundary::release(function)?;
    if status < 0 { Ok(error::publish_retained(&failure)) } else { Ok(UQA_OK) }
});
