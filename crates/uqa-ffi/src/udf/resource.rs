//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A successfully created host state is consumed before its one cleanup call.
#![allow(unsafe_code)]

use super::{
    registration::SharedData,
    support::{accept, sql_error},
};
use crate::{
    abi::UQAStatus,
    boundary,
    callback::Callbacks,
    error::{self, RetainedError, UQAError},
    session::Session,
};
use std::{
    ffi::c_void,
    sync::{Arc, Weak},
};
use uqa_core::memory::{MemoryBudget, MemoryReservation};
use uqa_sql::SQLError;

pub(super) type DestroyState = unsafe extern "C" fn(*mut c_void, *mut c_void) -> UQAStatus;

pub(super) struct Resource {
    state: Option<*mut c_void>,
    session: Weak<Session>,
    memory: MemoryBudget,
    pub(super) data: SharedData,
    destroy: DestroyState,
    cleanup: RetainedError,
    _reservation: MemoryReservation,
}
// C scan/group state can move between workers; each state's invocation is serialized.
unsafe impl Send for Resource {}

impl Resource {
    pub(super) fn new(
        session: &Arc<Session>,
        data: &SharedData,
        destroy: DestroyState,
        size: usize,
    ) -> boundary::Result<Self> {
        Ok(Self {
            state: None,
            session: Arc::downgrade(session),
            memory: session.memory.clone(),
            data: Arc::clone(data),
            destroy,
            cleanup: error::retained_slot(&session.memory)?,
            _reservation: session.memory.reserve(size)?,
        })
    }

    pub(super) fn created(&mut self, state: *mut c_void) {
        self.state = Some(state);
    }

    pub(super) fn pointer(&self) -> Result<*mut c_void, SQLError> {
        self.state
            .ok_or_else(|| sql_error(UQAError::state("function state is closed")))
    }

    pub(super) fn session(&self) -> Result<Arc<Session>, SQLError> {
        self.session.upgrade().ok_or_else(|| {
            sql_error(UQAError::state(
                "function state has no live invoking session",
            ))
        })
    }

    pub(super) fn close(&mut self) -> Result<(), SQLError> {
        let Some(state) = self.state.take() else {
            return Ok(());
        };
        let session = self.session.upgrade();
        let callbacks =
            Callbacks::retained(session.as_ref(), &self.memory, Arc::clone(&self.cleanup));
        let status = callbacks.invoke(|| unsafe { (self.destroy)(state, self.data.pointer) });
        accept(&callbacks, status, false)
    }
}

impl Drop for Resource {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
