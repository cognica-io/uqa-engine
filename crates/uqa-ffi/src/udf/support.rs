//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation resources and owned diagnostics shared by the C function adapters.

use std::{
    fmt,
    sync::{Arc, Weak},
};
use uqa_core::{
    memory::{BudgetedVec, Produced},
    Value,
};
use uqa_engine::SQLFunctionContext;
use uqa_sql::{ColumnType, SQLError};

use crate::{
    abi::{UQAStatus, UQA_DONE, UQA_OK},
    boundary,
    callback::Callbacks,
    error::{RetainedError, UQAError},
    session::Session,
    value::UQAValue,
};

#[derive(Debug)]
pub(crate) enum HostError {
    Owned(UQAError),
    Retained(RetainedError),
}

impl HostError {
    pub(crate) fn diagnostic(&self) -> Option<&UQAError> {
        match self {
            Self::Owned(error) => Some(error),
            Self::Retained(error) => error.get(),
        }
    }
    pub(crate) fn retained(&self) -> Option<&RetainedError> {
        match self {
            Self::Retained(error) => Some(error),
            Self::Owned(_) => None,
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let error = self.diagnostic();
        output.write_str(error.map_or("C function callback failed", UQAError::message))
    }
}
impl std::error::Error for HostError {}

pub(super) fn sql_error(mut error: UQAError) -> SQLError {
    // Library conversion/cancellation failures keep their native SQL identity;
    // host protocol failures have the external-routine cause instead.
    error
        .take_native()
        .unwrap_or_else(|| SQLError::Host(Arc::new(HostError::Owned(error))))
}

pub(super) fn accept(
    callbacks: &Callbacks<'_>,
    status: UQAStatus,
    done: bool,
) -> Result<(), SQLError> {
    if status == UQA_OK || (done && status == UQA_DONE) {
        return Ok(());
    }
    if status > 0 {
        callbacks.fail(UQAError::invalid(
            "unexpected positive function callback status",
        ));
    }
    if let Some(native) = callbacks.failure.get().and_then(UQAError::native) {
        return Err(native.clone());
    }
    Err(SQLError::Host(Arc::new(HostError::Retained(Arc::clone(
        &callbacks.failure,
    )))))
}

pub(super) fn session(context: &SQLFunctionContext) -> boundary::Result<Arc<Session>> {
    context
        .get::<Weak<Session>>()
        .and_then(Weak::upgrade)
        .ok_or_else(|| UQAError::state("C function has no live invoking session"))
}

pub(super) struct Arguments {
    _values: BudgetedVec<Box<UQAValue>>,
    _types: BudgetedVec<Option<Produced<ColumnType>>>,
    pointers: BudgetedVec<*const UQAValue>,
    count: u32,
}

impl Arguments {
    pub(super) fn new(session: &Arc<Session>, values: &[Value]) -> boundary::Result<Self> {
        let count = u32::try_from(values.len())
            .map_err(|_| UQAError::invalid("too many function arguments"))?;
        session.control().check()?;
        let mut views = BudgetedVec::new(&session.memory);
        let mut types = BudgetedVec::new(&session.memory);
        let mut pointers = BudgetedVec::new(&session.memory);
        views.reserve(values.len())?;
        types.reserve(values.len())?;
        pointers.reserve(values.len())?;
        for value in values {
            types.push(uqa_sql::type_resolution::runtime_value_type_with_control(
                value,
                Some(&session.native),
                &session.control(),
            )?)?;
        }
        for (value, ty) in values.iter().zip(types.iter()) {
            let view = UQAValue::borrowed(value, ty.as_deref(), session)?;
            pointers.push(std::ptr::from_ref(&*view))?;
            views.push(view)?;
        }
        Ok(Self {
            _values: views,
            _types: types,
            pointers,
            count,
        })
    }

    pub(super) fn pointer(&self) -> *const *const UQAValue {
        self.pointers.as_ptr()
    }
    pub(super) fn count(&self) -> u32 {
        self.count
    }
}
