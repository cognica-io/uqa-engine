//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole SQL messages and native atomic batches. The boundary does not split or reparse SQL.

use crate::{
    abi::{UQAStatus, UQA_ERROR_TYPE, UQA_OK},
    boundary,
    result::{BatchData, NativeResult, ResultData, UQABatchResult, UQAResult},
    session::{Session, UQAEngine},
    value::UQAValue,
};
use std::{ffi::c_char, sync::Arc};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLParam;

#[repr(C)]
pub struct UQASQLRequest {
    pub sql: *const c_char,
    pub params: *const *const UQAValue,
    pub param_count: u32,
}
#[repr(C)]
pub struct UQASQLRequestN {
    pub sql: *const c_char,
    pub sql_size: usize,
    pub params: *const *const UQAValue,
    pub param_count: u32,
}

// Pointer validity and source-owner lifetime come from the public argument contract.
#[allow(unsafe_code)]
pub(crate) unsafe fn parameters(
    session: &Arc<Session>,
    params: *const *const UQAValue,
    count: u32,
) -> boundary::Result<Produced<Vec<SQLParam>>> {
    let input = boundary::span(params, count as usize)?;
    let mut output = ProductionVec::new(session.control());
    output.reserve(input.len())?;
    for pointer in input {
        let value = boundary::input(*pointer)?;
        output.push_produced(parameter(session, value)?)?;
    }
    Ok(output.finish()?)
}

pub(crate) fn parameter(
    session: &Arc<Session>,
    value: &UQAValue,
) -> boundary::Result<Produced<SQLParam>> {
    let source = value
        .session
        .as_ref()
        .filter(|source| !Arc::ptr_eq(source, session));
    if value
        .ty()
        .is_some_and(uqa_sql::ast::ColumnType::is_catalog_dependent)
        && source.is_some_and(|source| !source.native.shares_database_with(&session.native))
    {
        return Err(crate::error::UQAError::new(
            UQA_ERROR_TYPE,
            "catalog-dependent value belongs to a different database",
        ));
    }
    let _source_call = source.map(|source| source.enter()).transpose()?;
    let original = source.map_or(&session.cancellation, |source| &source.cancellation);
    let control = ProductionControl::new(&session.memory, original, &session.cancellation);
    value.copy_parameter(&control)
}

// The SQL and parameter pointers may borrow the destination's previous contents. Copy first.
#[allow(unsafe_code)]
unsafe fn execute(
    engine: *mut UQAEngine,
    sql: &str,
    params: *const *const UQAValue,
    count: u32,
    result: *mut UQAResult,
) -> boundary::Result<UQAStatus> {
    boundary::input(result)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let params = parameters(session, params, count)?;
    let mut replacement = ResultData::prepare(session)?;
    replacement.native =
        NativeResult::Materialized(session.execute(|native| native.sql(sql, &params))?);
    boundary::input_mut(result)?.data = Some(replacement);
    Ok(UQA_OK)
}

ffi_fn!(uqa_engine_execute(engine: *mut UQAEngine, sql: *const c_char, result: *mut UQAResult) {
    execute(engine, boundary::text(sql)?, std::ptr::null(), 0, result)
});
ffi_fn!(uqa_engine_execute_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, result: *mut UQAResult) {
    execute(engine, boundary::text_n(sql, sql_size)?, std::ptr::null(), 0, result)
});
ffi_fn!(uqa_engine_execute_params(engine: *mut UQAEngine, sql: *const c_char, params: *const *const UQAValue, param_count: u32, result: *mut UQAResult) {
    execute(engine, boundary::text(sql)?, params, param_count, result)
});
ffi_fn!(uqa_engine_execute_params_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, params: *const *const UQAValue, param_count: u32, result: *mut UQAResult) {
    execute(engine, boundary::text_n(sql, sql_size)?, params, param_count, result)
});

struct Request<'a> {
    sql: &'a str,
    params: Produced<Vec<SQLParam>>,
}

fn batch(session: &Arc<Session>, requests: &[Request<'_>]) -> boundary::Result<BatchData> {
    let mut entries = BudgetedVec::new(&session.memory);
    entries.reserve(requests.len())?;
    let mut native_requests = BudgetedVec::new(&session.memory);
    native_requests.reserve(requests.len())?;
    for request in requests {
        entries.push(Some(ResultData::prepare(session)?))?;
        native_requests.push((request.sql, request.params.as_slice()))?;
    }
    let results = session.execute(|native| {
        native
            .sql_batch_diagnosed(&native_requests)
            .map_err(|error| {
                let mut diagnostic = crate::error::UQAError::from(error.error);
                diagnostic.statement_index = error.statement_index.map(|index| index as u64);
                diagnostic
            })
    })?;
    // All carriers are admitted before the native batch. Publication performs only ownership moves.
    for (entry, result) in entries.iter_mut().zip(results) {
        if let Some(entry) = entry {
            entry.native = NativeResult::Materialized(result);
        }
    }
    Ok(BatchData {
        entries,
        session: Arc::clone(session),
    })
}

ffi_fn!(uqa_engine_execute_batch(engine: *mut UQAEngine, requests: *const UQASQLRequest, count: usize, results: *mut UQABatchResult) {
    boundary::input(results)?;
    let input = boundary::span(requests, count)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let mut requests = BudgetedVec::new(&session.memory);
    requests.reserve(count)?;
    for request in input {
        requests.push(Request { sql: boundary::text(request.sql)?, params: parameters(session, request.params, request.param_count)? })?;
    }
    let replacement = batch(session, &requests)?;
    boundary::input_mut(results)?.data = Some(replacement);
    Ok(UQA_OK)
});
ffi_fn!(uqa_engine_execute_batch_n(engine: *mut UQAEngine, requests: *const UQASQLRequestN, count: usize, results: *mut UQABatchResult) {
    boundary::input(results)?;
    let input = boundary::span(requests, count)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let mut requests = BudgetedVec::new(&session.memory);
    requests.reserve(count)?;
    for request in input {
        requests.push(Request { sql: boundary::text_n(request.sql, request.sql_size)?, params: parameters(session, request.params, request.param_count)? })?;
    }
    let replacement = batch(session, &requests)?;
    boundary::input_mut(results)?.data = Some(replacement);
    Ok(UQA_OK)
});
