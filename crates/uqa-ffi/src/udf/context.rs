//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One result assignment per invocation, with owned value admission before publication.

use super::support::sql_error;
use crate::{
    abi::{UQA_DONE, UQA_ERROR_SQL, UQA_OK},
    boundary, callback,
    error::{RetainedError, UQAError},
    execute,
    session::Session,
    value::UQAValue,
};
use std::{ffi::c_char, sync::Arc};
use uqa_core::Value;
use uqa_sql::SQLError;

pub struct UQAUDFContext {
    session: Arc<Session>,
    failure: RetainedError,
    result: Option<Box<UQAValue>>,
}

impl UQAUDFContext {
    pub(super) fn new(session: &Arc<Session>, failure: &RetainedError) -> Self {
        Self {
            session: Arc::clone(session),
            failure: Arc::clone(failure),
            result: None,
        }
    }

    pub(super) fn finish(self) -> Result<Value, SQLError> {
        let result = self.result.ok_or_else(|| {
            sql_error(UQAError::state("function callback did not assign a result"))
        })?;
        // The native callback boundary adopts the existing external value immediately.
        Ok((*result).into_native().map_err(sql_error)?.into_parts().0)
    }
}

ffi_fn!(uqa_udf_context_get_result(context: *mut UQAUDFContext, out: *mut *const UQAValue) {
    let out = boundary::output(out)?;
    let context = boundary::input(context)?;
    callback::check_scope(&context.failure)?;
    match &context.result {
        Some(value) => { *out = std::ptr::from_ref(&**value); Ok(UQA_OK) }
        None => Ok(UQA_DONE),
    }
});

ffi_fn!(uqa_udf_context_set_result(context: *mut UQAUDFContext, value: *const UQAValue) {
    let context = boundary::input_mut(context)?;
    callback::check_scope(&context.failure)?;
    if context.result.is_some() { return Err(UQAError::state("function result is already assigned")); }
    let value = boundary::input(value)?;
    let value = execute::parameter(&context.session, value)?;
    let value = UQAValue::owned(value, Some(Arc::clone(&context.session)))?;
    context.result = Some(value);
    Ok(UQA_OK)
});

macro_rules! context_failure {
    ($api:ident, $read:ident $(, $size:ident)?) => {
        ffi_fn!($api(context: *mut UQAUDFContext, message: *const c_char $(, $size: usize)?) {
            let context = boundary::input(context)?;
            callback::check_scope(&context.failure)?;
            callback::fail(UQA_ERROR_SQL, boundary::$read(message $(, $size)?)?)
        });
    };
}
context_failure!(uqa_udf_context_fail, text);
context_failure!(uqa_udf_context_fail_n, required_text_n, message_size);
