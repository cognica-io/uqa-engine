//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible C aggregate factories and non-consuming result reads.
#![allow(unsafe_code)]

use super::{
    registration::{options, registration_error, SharedData},
    resource::Resource,
    support::{self, sql_error, Arguments},
    UQAUDFContext, UQAUDFOptions, UQAUserDataDestroyFn, UQAUDF,
};
use crate::{
    abi::{UQAStatus, UQA_OK},
    boundary,
    callback::Callbacks,
    error::UQAError,
    session::UQAEngine,
    value::UQAValue,
};
use std::{
    ffi::{c_char, c_void},
    sync::Arc,
};
use uqa_core::Value;
use uqa_engine::{SQLAggregateFactory, SQLAggregateState, SQLFunctionContext};
use uqa_sql::SQLError;

pub type UQAUDFCreateStateFn = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> UQAStatus;
pub type UQAUDFObserveFn =
    unsafe extern "C" fn(*mut c_void, *const *const UQAValue, u32, *mut c_void) -> UQAStatus;
pub type UQAUDFFinishFn =
    unsafe extern "C" fn(*mut UQAUDFContext, *mut c_void, *mut c_void) -> UQAStatus;
pub type UQAUDFDestroyStateFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> UQAStatus;

struct Aggregate {
    create: UQAUDFCreateStateFn,
    observe: UQAUDFObserveFn,
    finish: UQAUDFFinishFn,
    destroy: UQAUDFDestroyStateFn,
    data: SharedData,
}

struct State {
    resource: Resource,
    observe: UQAUDFObserveFn,
    finish: UQAUDFFinishFn,
}

impl SQLAggregateFactory for Aggregate {
    fn try_create_state_with_context(
        &self,
        native: &SQLFunctionContext,
    ) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        let session = support::session(native).map_err(sql_error)?;
        session.control().check().map_err(SQLError::from)?;
        let mut state = boundary::boxed(State {
            resource: Resource::new(&session, &self.data, self.destroy, size_of::<State>())
                .map_err(sql_error)?,
            observe: self.observe,
            finish: self.finish,
        })
        .map_err(sql_error)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let mut pointer = std::ptr::null_mut();
        let status =
            callbacks.invoke(|| unsafe { (self.create)(self.data.pointer, &raw mut pointer) });
        support::accept(&callbacks, status, false)?;
        state.resource.created(pointer);
        Ok(state)
    }
}

impl SQLAggregateState for State {
    fn observe(&mut self, values: &[Value]) -> Result<(), SQLError> {
        let pointer = self.resource.pointer()?;
        let session = self.resource.session()?;
        let arguments = Arguments::new(&session, values).map_err(sql_error)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let status = callbacks.invoke(|| unsafe {
            (self.observe)(
                pointer,
                arguments.pointer(),
                arguments.count(),
                self.resource.data.pointer,
            )
        });
        support::accept(&callbacks, status, false)
    }

    fn finish(&self) -> Result<Value, SQLError> {
        let state = self.resource.pointer()?;
        let session = self.resource.session()?;
        session.control().check().map_err(SQLError::from)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let mut context = UQAUDFContext::new(&session, &callbacks.failure);
        let pointer = &raw mut context;
        let status = callbacks
            .invoke(|| unsafe { (self.finish)(pointer, state, self.resource.data.pointer) });
        support::accept(&callbacks, status, false)?;
        context.finish()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.resource.close()
    }
}

ffi_fn!(uqa_udf_register_aggregate(engine: *mut UQAEngine, name: *const c_char, options_pointer: *mut UQAUDFOptions, create_state: Option<UQAUDFCreateStateFn>, observe: Option<UQAUDFObserveFn>, finish: Option<UQAUDFFinishFn>, destroy_state: Option<UQAUDFDestroyStateFn>, user_data: *mut c_void, destroy_user_data: Option<UQAUserDataDestroyFn>, out: *mut *mut UQAUDF) {
    let out = boundary::new_handle(out)?;
    let name = boundary::text(name)?;
    let create = create_state.ok_or_else(|| UQAError::invalid("aggregate create_state must not be NULL"))?;
    let observe = observe.ok_or_else(|| UQAError::invalid("aggregate observe must not be NULL"))?;
    let finish = finish.ok_or_else(|| UQAError::invalid("aggregate finish must not be NULL"))?;
    let destroy = destroy_state.ok_or_else(|| UQAError::invalid("aggregate destroy_state must not be NULL"))?;
    let options = options(options_pointer)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let handle = UQAUDF::prepare(session, user_data, destroy_user_data)?;
    let native = session.native.register_aggregate_function_retained(name, options, Aggregate { create, observe, finish, destroy, data: Arc::clone(&handle.data) }).map_err(registration_error)?;
    *out = handle.publish(native);
    Ok(UQA_OK)
});
