//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental consumption of native completed query spills with independent retained results.

use std::{
    cell::RefCell,
    ffi::c_char,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
};
use uqa_core::memory::{Budgeted, MemoryReservation};
use uqa_engine::{ColumnarBatch, SQLCursor};

use crate::{
    abi::{
        UQAStatus, UQAStringView, UQA_COMPLETION_COMMITTED, UQA_COMPLETION_IN_TRANSACTION,
        UQA_DONE, UQA_ERROR_BUSY, UQA_ERROR_IO, UQA_ERROR_PANIC, UQA_ERROR_RANGE, UQA_OK,
    },
    boundary,
    error::{self, RetainedError, UQAError},
    execute::parameters,
    result::{cursor_columns, CursorColumns, CursorSlice, NativeResult, ResultData, UQAResult},
    session::{Session, UQAEngine},
    types::{self, UQAType},
    value::UQAValue,
};

pub struct UQACursor {
    columns: Option<CursorColumns>,
    row_count: usize,
    completion: u32,
    failure: RetainedError,
    session: Arc<Session>,
    state: RefCell<CursorState>,
    _memory: MemoryReservation,
}

#[derive(Default)]
struct CursorState {
    native: Option<SQLCursor>,
    batch: Option<Arc<Budgeted<ColumnarBatch>>>,
    offset: usize,
    delivered: usize,
}

impl UQACursor {
    #[cfg(feature = "arrow-export")]
    pub(crate) fn fetch_arrow(
        &self,
        max_rows: u32,
        policy: u32,
        schema: &mut crate::arrow::UQAArrowSchema,
        array: &mut crate::arrow::UQAArrowArray,
    ) -> boundary::Result<UQAStatus> {
        let _call = self.session.enter_cleanup()?;
        if self.failure.get().is_some() {
            return Ok(error::publish_retained(&self.failure));
        }
        self.session.check_poison()?;
        let mut state = self
            .state
            .try_borrow_mut()
            .map_err(|_| UQAError::new(UQA_ERROR_BUSY, "cursor already has an active fetch"))?;
        match catch_unwind(AssertUnwindSafe(|| {
            let Some(data) = self.fetch(&mut state, max_rows)? else {
                return Ok(UQA_DONE);
            };
            crate::arrow::export::publish(&data, policy, schema, array)
        })) {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Ok(self.fail(&mut state, error)),
            Err(_) => {
                self.session.poison();
                Ok(self.fail(
                    &mut state,
                    UQAError::new(UQA_ERROR_PANIC, "panic while exporting a native cursor"),
                ))
            }
        }
    }

    fn columns(&self) -> &CursorColumns {
        self.columns
            .as_ref()
            .expect("published cursor has its descriptor")
    }

    fn column(&self, column: u32) -> boundary::Result<usize> {
        let column = column as usize;
        if column >= self.columns().len() {
            return Err(UQAError::new(
                UQA_ERROR_RANGE,
                "column index is out of range",
            ));
        }
        Ok(column)
    }

    fn fetch(
        &self,
        state: &mut CursorState,
        max_rows: u32,
    ) -> boundary::Result<Option<Box<ResultData>>> {
        if state.delivered == self.row_count {
            state.native = None;
            return Ok(None);
        }
        self.session.control().check()?;
        let mut result = ResultData::prepare(&self.session)?;
        if state.batch.is_none() {
            let batch = state
                .native
                .as_mut()
                .and_then(Iterator::next)
                .ok_or_else(|| {
                    UQAError::new(
                        UQA_ERROR_IO,
                        "native cursor ended before its declared row count",
                    )
                })??;
            self.session.control().check()?;
            // The native owner retains batch payloads under its existing contract. Only the C
            // sharing carrier is charged here; copying rows would duplicate native retention.
            state.batch =
                Some(Budgeted::new(batch, self.session.memory.empty_reservation()).into_shared()?);
            state.offset = 0;
        }
        let batch = state
            .batch
            .as_ref()
            .expect("cursor has a nonempty native batch");
        let count = (max_rows as usize).min(batch.len() - state.offset);
        if count == 0 || count > self.row_count - state.delivered {
            return Err(UQAError::new(
                UQA_ERROR_IO,
                "native cursor row count does not match its batch",
            ));
        }
        let end = state.offset + count;
        result.native = NativeResult::Columnar(CursorSlice {
            columns: Arc::clone(self.columns()),
            batch: Arc::clone(batch),
            rows: state.offset..end,
        });
        state.delivered += count;
        state.offset = end;
        if end == batch.len() {
            state.batch = None;
            state.offset = 0;
        }
        if state.delivered == self.row_count {
            state.native = None;
        }
        Ok(Some(result))
    }

    fn fail(&self, state: &mut CursorState, mut error: UQAError) -> UQAStatus {
        error.completion = self.completion;
        let _ = self.failure.set(error);
        // A failed fetch cannot be resumed. Release remaining rows and the native temporary file.
        state.native = None;
        state.batch = None;
        error::publish_retained(&self.failure)
    }
}

// SQL and parameter pointers are borrowed under the public C lifetime contract.
#[allow(unsafe_code)]
unsafe fn open(
    engine: *mut UQAEngine,
    sql: &str,
    params: *const *const UQAValue,
    count: u32,
    out: *mut *mut UQACursor,
) -> boundary::Result<UQAStatus> {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let params = parameters(session, params, count)?;
    let memory = session.memory.reserve(size_of::<UQACursor>())?;
    let mut cursor = boundary::boxed(UQACursor {
        columns: None,
        row_count: 0,
        completion: UQA_COMPLETION_IN_TRANSACTION,
        failure: error::retained_slot(&session.memory)?,
        session: Arc::clone(session),
        state: RefCell::new(CursorState::default()),
        _memory: memory,
    })?;
    let native = session
        .native
        .sql_cursor(sql, &params)
        .map_err(|error| session.execution_error(error))?;
    // The successful native cursor path completed evaluation and any implicit transaction.
    // Record that fact now; unrelated subsequent session work must not rewrite it.
    if session.native.transaction_depth() == 0 {
        cursor.completion = UQA_COMPLETION_COMMITTED;
    }
    cursor.columns = Some(cursor_columns(&native, session).map_err(|mut error| {
        error.completion = cursor.completion;
        error
    })?);
    cursor.row_count = native.row_count();
    if cursor.row_count != 0 {
        cursor.state.get_mut().native = Some(native);
    }
    *out = Box::into_raw(cursor);
    Ok(UQA_OK)
}

ffi_fn!(uqa_cursor_open(engine: *mut UQAEngine, sql: *const c_char, out: *mut *mut UQACursor) {
    open(engine, boundary::text(sql)?, std::ptr::null(), 0, out)
});
ffi_fn!(uqa_cursor_open_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, out: *mut *mut UQACursor) {
    open(engine, boundary::text_n(sql, sql_size)?, std::ptr::null(), 0, out)
});
ffi_fn!(uqa_cursor_open_params(engine: *mut UQAEngine, sql: *const c_char, params: *const *const UQAValue, param_count: u32, out: *mut *mut UQACursor) {
    open(engine, boundary::text(sql)?, params, param_count, out)
});
ffi_fn!(uqa_cursor_open_params_n(engine: *mut UQAEngine, sql: *const c_char, sql_size: usize, params: *const *const UQAValue, param_count: u32, out: *mut *mut UQACursor) {
    open(engine, boundary::text_n(sql, sql_size)?, params, param_count, out)
});
ffi_fn!(uqa_cursor_fetch(cursor: *mut UQACursor, max_rows: u32, result: *mut UQAResult) {
    boundary::input(result)?;
    if max_rows == 0 {
        return Err(UQAError::invalid("max_rows must be positive"));
    }
    let cursor = boundary::input(cursor)?;
    let session = Arc::clone(&cursor.session);
    let _call = session.enter_cleanup()?;
    if cursor.failure.get().is_some() {
        return Ok(error::publish_retained(&cursor.failure));
    }
    session.check_poison()?;
    // Acquire mutable cursor state only after session admission; concurrent callers can inspect
    // the immutable session reference and receive BUSY without creating aliased mutable borrows.
    let mut state = cursor.state.try_borrow_mut()
        .map_err(|_| UQAError::new(UQA_ERROR_BUSY, "cursor already has an active fetch"))?;
    // A fetch panic must poison the session and become this cursor's retained terminal failure.
    // The outer ABI catch still protects validation and publication outside this scope.
    match catch_unwind(AssertUnwindSafe(|| cursor.fetch(&mut state, max_rows))) {
        Ok(Ok(Some(replacement))) => {
            boundary::input_mut(result)?.data = Some(replacement);
            Ok(UQA_OK)
        },
        Ok(Ok(None)) => Ok(UQA_DONE),
        Ok(Err(error)) => Ok(cursor.fail(&mut state, error)),
        Err(_) => {
            session.poison();
            Ok(cursor.fail(&mut state, UQAError::new(UQA_ERROR_PANIC, "panic while consuming a native cursor")))
        },
    }
});
ffi_fn!(uqa_cursor_get_row_count(cursor: *mut UQACursor, out: *mut u64) {
    let out = boundary::output(out)?;
    *out = boundary::input(cursor)?.row_count as u64;
    Ok(UQA_OK)
});
ffi_fn!(uqa_cursor_get_column_count(cursor: *mut UQACursor, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = u32::try_from(boundary::input(cursor)?.columns().len())
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "column count exceeds the C range"))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_cursor_get_column_name(cursor: *mut UQACursor, column: u32, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let cursor = boundary::input(cursor)?;
    *out = cursor.columns()[cursor.column(column)?].name.as_str().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_cursor_get_column_type(cursor: *mut UQACursor, column: u32, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let cursor = boundary::input(cursor)?;
    match cursor.columns()[cursor.column(column)?].ty.as_deref() {
        Some(ty) => { *out = types::view(ty); Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_cursor_close(cursor: *mut *mut UQACursor) {
    if cursor.is_null() { return Ok(UQA_OK); }
    let pointer = *boundary::input(cursor)?;
    if pointer.is_null() { return Ok(UQA_OK); }
    let session = Arc::clone(&boundary::input(pointer)?.session);
    let _call = session.enter_cleanup()?;
    boundary::release(cursor)
});
