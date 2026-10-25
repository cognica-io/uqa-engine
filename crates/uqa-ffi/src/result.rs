//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reusable result destinations keep native rows and lazily allocate stable borrowed cell views.

use crate::{
    abi::{
        UQAStringView, UQA_DONE, UQA_ERROR_BUSY, UQA_ERROR_RANGE, UQA_OK, UQA_RESULT_COMMAND,
        UQA_RESULT_ROWS, UQA_RESULT_UNKNOWN,
    },
    boundary,
    error::UQAError,
    session::Session,
    types::{self, UQAType},
    value::UQAValue,
};
use std::{cell::RefCell, sync::Arc};
use uqa_core::memory::{BudgetedMap, BudgetedVec, MemoryReservation};
use uqa_sql::{SQLResult, SQLResultKind};

mod columnar;
pub(crate) use columnar::{columns as cursor_columns, CursorColumns, CursorSlice};

pub(crate) enum NativeResult {
    Materialized(SQLResult),
    Columnar(CursorSlice),
}

impl NativeResult {
    pub(crate) fn row_count(&self) -> usize {
        match self {
            Self::Materialized(result) => result.rows.len(),
            Self::Columnar(result) => result.rows.len(),
        }
    }

    pub(crate) fn column_count(&self) -> usize {
        match self {
            Self::Materialized(result) => result.columns.len(),
            Self::Columnar(result) => result.columns.len(),
        }
    }

    pub(crate) fn column_name(&self, column: usize) -> &str {
        match self {
            Self::Materialized(result) => &result.columns[column],
            Self::Columnar(result) => &result.columns[column].name,
        }
    }

    pub(crate) fn column_type(&self, column: usize) -> Option<&uqa_sql::ColumnType> {
        match self {
            Self::Materialized(result) => result.column_types.get(column).and_then(Option::as_ref),
            Self::Columnar(result) => result.columns[column].ty.as_deref(),
        }
    }

    pub(crate) fn value_at(&self, row: usize, column: usize) -> Option<&uqa_core::Value> {
        match self {
            Self::Materialized(result) => result.value_at(row, column),
            Self::Columnar(result) => result.value_at(row, column),
        }
    }
}

#[derive(Default)]
pub struct UQAResult {
    pub(crate) data: Option<Box<ResultData>>,
}

pub(crate) struct ResultData {
    pub(crate) native: NativeResult,
    types: types::TypeViews,
    cells: RefCell<BudgetedMap<(u64, u32), Box<UQAValue>>>,
    pub(crate) session: Arc<Session>,
    _memory: MemoryReservation,
}

impl ResultData {
    /// Allocate the C publication carrier before SQL admission. Completing it only moves native data.
    pub(crate) fn prepare(session: &Arc<Session>) -> boundary::Result<Box<Self>> {
        let memory = session.memory.reserve(size_of::<Self>())?;
        boundary::boxed(Self {
            native: NativeResult::Materialized(SQLResult::empty()),
            types: types::TypeViews::new(&session.control()),
            cells: RefCell::new(BudgetedMap::new(&session.memory)),
            session: Arc::clone(session),
            _memory: memory,
        })
    }

    fn check_column(&self, column: u32) -> boundary::Result<usize> {
        let column = column as usize;
        if column >= self.native.column_count() {
            return Err(UQAError::new(
                UQA_ERROR_RANGE,
                "column index is out of range",
            ));
        }
        Ok(column)
    }
}

impl UQAResult {
    pub(crate) fn data(&self) -> boundary::Result<&ResultData> {
        self.data
            .as_deref()
            .ok_or_else(|| UQAError::state("result has not been populated"))
    }
}

ffi_fn!(uqa_result_create(out: *mut *mut UQAResult) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQAResult::default())?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_destroy(result: *mut *mut UQAResult) { boundary::release(result) });
ffi_fn!(uqa_result_get_row_count(result: *mut UQAResult, out: *mut u64) {
    let out = boundary::output(out)?;
    *out = boundary::input(result)?.data()?.native.row_count() as u64;
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_get_column_count(result: *mut UQAResult, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = u32::try_from(boundary::input(result)?.data()?.native.column_count())
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "column count exceeds the C range"))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_get_column_name(result: *mut UQAResult, column: u32, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let data = boundary::input(result)?.data()?;
    *out = data.native.column_name(data.check_column(column)?).into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_get_column_type(result: *mut UQAResult, column: u32, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    let data = boundary::input(result)?.data()?;
    let _call = data.session.enter_value()?;
    match data.native.column_type(data.check_column(column)?) {
        Some(ty) => { *out = data.types.get(u64::from(column), ty, Some(&data.session))?; Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_result_get_value(result: *mut UQAResult, row: u64, column: u32, out: *mut *const UQAValue) {
    let out = boundary::output(out)?;
    let data = boundary::input(result)?.data()?;
    let _call = data.session.enter()?;
    let column_index = data.check_column(column)?;
    let row_index = usize::try_from(row).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "row index exceeds the addressable range"))?;
    let value = data.native.value_at(row_index, column_index).ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "row index is out of range"))?;
    let mut cells = data.cells.try_borrow_mut().map_err(|_| UQAError::new(UQA_ERROR_BUSY, "result is already being observed"))?;
    if let Some(value) = cells.get(&(row, column)) { *out = std::ptr::from_ref(&**value); return Ok(UQA_OK); }
    let ty = data.native.column_type(column_index);
    let value = UQAValue::borrowed(value, ty, &data.session)?;
    let pointer = std::ptr::from_ref(&*value);
    let entry = cells.prepare_entry((row, column), value)?;
    cells.insert_prepared(entry);
    *out = pointer;
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_get_affected_rows(result: *mut UQAResult, out: *mut u64) {
    let out = boundary::output(out)?;
    *out = match &boundary::input(result)?.data()?.native {
        NativeResult::Materialized(result) => result.affected_rows,
        NativeResult::Columnar(_) => 0,
    };
    Ok(UQA_OK)
});
ffi_fn!(uqa_result_get_command_tag(result: *mut UQAResult, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let tag = match &boundary::input(result)?.data()?.native {
        NativeResult::Materialized(result) => result.command_tag.as_ref(),
        NativeResult::Columnar(_) => None,
    };
    match tag {
        Some(tag) => { *out = tag.as_str().into(); Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_result_get_kind(result: *mut UQAResult, out: *mut u32) {
    let out = boundary::output(out)?;
    let kind = match &boundary::input(result)?.data()?.native {
        NativeResult::Materialized(result) => result.kind,
        NativeResult::Columnar(_) => SQLResultKind::Rows,
    };
    *out = match kind {
        SQLResultKind::Command => UQA_RESULT_COMMAND,
        SQLResultKind::Rows => UQA_RESULT_ROWS,
        SQLResultKind::Unknown => UQA_RESULT_UNKNOWN,
    };
    Ok(UQA_OK)
});

#[derive(Default)]
pub struct UQABatchResult {
    pub(crate) data: Option<BatchData>,
}
pub(crate) struct BatchData {
    pub(crate) entries: BudgetedVec<Option<Box<ResultData>>>,
    pub(crate) session: Arc<Session>,
}
ffi_fn!(uqa_batch_result_create(out: *mut *mut UQABatchResult) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQABatchResult::default())?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_batch_result_destroy(batch: *mut *mut UQABatchResult) { boundary::release(batch) });
ffi_fn!(uqa_batch_result_get_count(batch: *mut UQABatchResult, out: *mut u64) {
    let out = boundary::output(out)?;
    *out = boundary::input(batch)?.data.as_ref().ok_or_else(|| UQAError::state("batch has not been populated"))?.entries.len() as u64;
    Ok(UQA_OK)
});
ffi_fn!(uqa_batch_result_take(batch: *mut UQABatchResult, index: u64, result: *mut UQAResult) {
    let index = usize::try_from(index).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "batch index exceeds the addressable range"))?;
    let result = boundary::input_mut(result)?;
    let data = boundary::input_mut(batch)?.data.as_mut().ok_or_else(|| UQAError::state("batch has not been populated"))?;
    let entry = data.entries.get_mut(index).ok_or_else(|| UQAError::new(UQA_ERROR_RANGE, "batch index is out of range"))?;
    if entry.is_none() { return Err(UQAError::state("batch member was already taken")); }
    let session = Arc::clone(&data.session);
    let _call = session.enter()?;
    result.data = entry.take();
    Ok(UQA_OK)
});
