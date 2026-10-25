//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar trampoline; SQL continues to own evaluation and external-value retention.
#![allow(unsafe_code)]

use super::{
    registration::{options, registration_error, SharedData},
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
use uqa_engine::{SQLFunctionContext, SQLScalarFunction};
use uqa_sql::SQLError;

pub type UQAUDFScalarFn =
    unsafe extern "C" fn(*mut UQAUDFContext, *const *const UQAValue, u32, *mut c_void) -> UQAStatus;

struct Scalar {
    function: UQAUDFScalarFn,
    data: SharedData,
}

impl SQLScalarFunction for Scalar {
    fn call_with_context(
        &self,
        native: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<Value, SQLError> {
        let session = support::session(native).map_err(sql_error)?;
        let arguments = Arguments::new(&session, args).map_err(sql_error)?;
        let callbacks = Callbacks::new(&session).map_err(sql_error)?;
        let mut context = UQAUDFContext::new(&session, &callbacks.failure);
        let pointer = &raw mut context;
        let status = callbacks.invoke(|| unsafe {
            (self.function)(
                pointer,
                arguments.pointer(),
                arguments.count(),
                self.data.pointer,
            )
        });
        support::accept(&callbacks, status, false)?;
        context.finish()
    }
}

macro_rules! scalar_registration {
    ($api:ident, $read:ident $(, $size:ident)?) => {
        ffi_fn!($api(engine: *mut UQAEngine, name: *const c_char, $($size: usize,)? options_pointer: *mut UQAUDFOptions, function: Option<UQAUDFScalarFn>, user_data: *mut c_void, destroy_user_data: Option<UQAUserDataDestroyFn>, out: *mut *mut UQAUDF) {
            let out = boundary::new_handle(out)?;
            let name = boundary::$read(name $(, $size)?)?;
            let function = function.ok_or_else(|| UQAError::invalid("scalar callback must not be NULL"))?;
            let options = options(options_pointer)?;
            let session = &boundary::input(engine)?.session;
            let _call = session.enter()?;
            let handle = UQAUDF::prepare(session, user_data, destroy_user_data)?;
            let native = session.native.register_scalar_function_retained(name, options, Scalar { function, data: Arc::clone(&handle.data) }).map_err(registration_error)?;
            *out = handle.publish(native);
            Ok(UQA_OK)
        });
    };
}
scalar_registration!(uqa_udf_register_scalar, text);
scalar_registration!(uqa_udf_register_scalar_n, required_text_n, name_size);
